//! The menu bar icon (an `NSStatusItem`) and its menu, built from plain `MenuEntry` lists so the
//! overlay decides what the menu holds (`overlay::menu_bar`) and this file only talks to AppKit.
//! The menu is rebuilt each time it opens (`menuNeedsUpdate:`), so it always shows the current
//! state; a chosen item hands its command back. Also the alert that asks before the icon is hidden.
//! Everything here runs on the main thread.

use std::cell::RefCell;
use std::rc::Rc;

use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol, ProtocolObject};
use objc2::{AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSApplication, NSBitmapImageRep, NSControlStateValueOff, NSControlStateValueOn, NSEventModifierFlags, NSImage,
    NSMenu, NSMenuDelegate, NSMenuItem, NSSquareStatusItemLength, NSStatusBar, NSStatusItem,
};
use objc2_foundation::{NSData, NSObject, NSOperatingSystemVersion, NSProcessInfo, NSSize, NSString, ns_string};

/// The icon's size in points; the PNGs are drawn for 1× and 2× screens.
const ICON_POINTS: f64 = 18.0;
const IDLE: [&[u8]; 2] = [include_bytes!("../../assets/menu-bar/status-idle.png"), include_bytes!("../../assets/menu-bar/status-idle@2x.png")];
const LIVE: [&[u8]; 2] = [include_bytes!("../../assets/menu-bar/status-live.png"), include_bytes!("../../assets/menu-bar/status-live@2x.png")];

/// A line of the menu. `C` is what choosing an item means to the caller.
#[derive(Clone, Debug, PartialEq)]
pub enum MenuEntry<C> {
    Item { title: String, command: C, enabled: bool, checked: bool, key: Option<MenuKey> },
    /// Greyed-out text that can't be chosen, such as a status line.
    Label(String),
    /// A group's title inside a list.
    Header(String),
    Submenu { title: String, entries: Vec<MenuEntry<C>> },
    Separator,
}

impl<C> MenuEntry<C> {
    /// An item that can be chosen, unchecked and without a shortcut.
    pub fn item(title: impl Into<String>, command: C) -> Self { Self::Item { title: title.into(), command, enabled: true, checked: false, key: None } }

    pub fn key(mut self, shortcut: MenuKey) -> Self {
        if let Self::Item { key, .. } = &mut self { *key = Some(shortcut); }
        self
    }

    pub fn checked(mut self, on: bool) -> Self {
        if let Self::Item { checked, .. } = &mut self { *checked = on; }
        self
    }

    pub fn enabled(mut self, on: bool) -> Self {
        if let Self::Item { enabled, .. } = &mut self { *enabled = on; }
        self
    }
}

/// A shortcut shown beside an item. The menu bar's menu only answers to it while it is open; the
/// app's own shortcuts work the same as always.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MenuKey { pub key: &'static str, pub command: bool, pub shift: bool, pub option: bool, pub control: bool }

/// Fills the menu as it opens and runs a chosen item's tag.
struct Handlers {
    fill: Fill,
    chosen: Box<dyn Fn(isize)>,
}

type Fill = Box<dyn Fn(&NSMenu, &Target)>;

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and `Target` does not implement Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "CluelyRSMenuBarTarget"]
    #[ivars = Handlers]
    struct Target;

    impl Target {
        #[unsafe(method(menuItemClicked:))]
        fn menu_item_clicked(&self, item: &NSMenuItem) { (self.ivars().chosen)(item.tag()); }
    }

    unsafe impl NSObjectProtocol for Target {}

    unsafe impl NSMenuDelegate for Target {
        #[unsafe(method(menuNeedsUpdate:))]
        fn menu_needs_update(&self, menu: &NSMenu) { (self.ivars().fill)(menu, self); }
    }
);

impl Target {
    fn new(handlers: Handlers, mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(handlers);
        // SAFETY: NSObject's designated initializer.
        unsafe { msg_send![super(this), init] }
    }
}

/// The menu bar icon and its menu. The menu and its target stay alive as long as this does
/// (menu items and the menu hold them weakly); the icon itself comes and goes with `set_shown`.
pub struct StatusMenu {
    _target: Retained<Target>,
    menu: Retained<NSMenu>,
    item: Option<Retained<NSStatusItem>>,
    live: bool,
    images: [Retained<NSImage>; 2],
}

impl StatusMenu {
    /// `entries` describes the menu each time it opens; `chosen` gets the command of the item
    /// picked. AppKit calls both from its own event handling (the status bar isn't a GPUI window),
    /// never from inside a GPUI update. The icon is hidden until `set_shown(true)`.
    pub fn new<C: Clone + 'static>(entries: impl Fn() -> Vec<MenuEntry<C>> + 'static, chosen: impl Fn(C) + 'static) -> Option<Self> {
        let mtm = MainThreadMarker::new()?;
        // The commands of the items on screen, by tag.
        let shown: Rc<RefCell<Vec<C>>> = Rc::default();
        let commands = shown.clone();
        let refill = move |menu: &NSMenu, target: &Target| {
            let mut commands = commands.borrow_mut();
            commands.clear();
            fill(menu, &entries(), &mut commands, target, mtm);
        };
        let chosen = move |tag: isize| {
            // Cloned out first: `chosen` may run a modal alert, during which the menu can open again.
            let command = usize::try_from(tag).ok().and_then(|tag| shown.borrow().get(tag).cloned());
            if let Some(command) = command { chosen(command); }
        };
        let target = Target::new(Handlers { fill: Box::new(refill), chosen: Box::new(chosen) }, mtm);
        let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), ns_string!("CluelyRS"));
        menu.setAutoenablesItems(false);
        menu.setDelegate(Some(ProtocolObject::from_ref(&*target)));
        Some(Self { _target: target, menu, item: None, live: false, images: [template_image(IDLE), template_image(LIVE)] })
    }

    /// Add the icon to the menu bar, or take it away.
    pub fn set_shown(&mut self, shown: bool) {
        if shown == self.item.is_some() { return; }
        let bar = NSStatusBar::systemStatusBar();
        match self.item.take() {
            Some(item) => bar.removeStatusItem(&item),
            None => {
                let item = bar.statusItemWithLength(NSSquareStatusItemLength);
                // With a menu and no action, a click opens the menu; the icon does nothing else.
                item.setMenu(Some(&self.menu));
                self.item = Some(item);
                self.show_state();
            }
        }
    }

    /// While Live is on, the icon has a dot in the middle and its tooltip says so.
    pub fn set_live(&mut self, live: bool) {
        if live == self.live { return; }
        self.live = live;
        self.show_state();
    }

    fn show_state(&self) {
        let (Some(item), Some(mtm)) = (&self.item, MainThreadMarker::new()) else { return };
        let Some(button) = item.button(mtm) else { return };
        button.setImage(Some(&self.images[self.live as usize]));
        button.setToolTip(Some(&NSString::from_str(if self.live { "CluelyRS · Live" } else { "CluelyRS" })));
    }
}

/// Replace `menu`'s items with `entries`, recording each item's command at the index its tag holds.
fn fill<C: Clone>(menu: &NSMenu, entries: &[MenuEntry<C>], commands: &mut Vec<C>, target: &Target, mtm: MainThreadMarker) {
    menu.removeAllItems();
    for entry in entries {
        let item = match entry {
            MenuEntry::Separator => NSMenuItem::separatorItem(mtm),
            MenuEntry::Label(title) => disabled_item(title, mtm),
            MenuEntry::Header(title) => header_item(title, mtm),
            MenuEntry::Item { title, command, enabled, checked, key } => {
                // SAFETY: `menuItemClicked:` is the target's method and takes the item.
                let item = unsafe { NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), &NSString::from_str(title), Some(sel!(menuItemClicked:)), ns_string!("")) };
                let target: &AnyObject = target;
                // SAFETY: the target outlives the menu's items (`StatusMenu` owns both).
                unsafe { item.setTarget(Some(target)) };
                item.setTag(commands.len() as isize);
                commands.push(command.clone());
                item.setEnabled(*enabled);
                item.setState(if *checked { NSControlStateValueOn } else { NSControlStateValueOff });
                if let Some(key) = key {
                    item.setKeyEquivalent(&NSString::from_str(key.key));
                    item.setKeyEquivalentModifierMask(modifiers(key));
                }
                item
            }
            MenuEntry::Submenu { title, entries } => {
                let item = plain_item(title, mtm);
                let submenu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(title));
                submenu.setAutoenablesItems(false);
                fill(&submenu, entries, commands, target, mtm);
                item.setSubmenu(Some(&submenu));
                item
            }
        };
        menu.addItem(&item);
    }
}

fn plain_item(title: &str, mtm: MainThreadMarker) -> Retained<NSMenuItem> {
    // SAFETY: no action.
    unsafe { NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), &NSString::from_str(title), None, ns_string!("")) }
}

/// macOS 14's section header (a small title, the group's items set in under it); before that, a
/// greyed-out item.
fn header_item(title: &str, mtm: MainThreadMarker) -> Retained<NSMenuItem> {
    let sonoma = NSOperatingSystemVersion { majorVersion: 14, minorVersion: 0, patchVersion: 0 };
    if NSProcessInfo::processInfo().isOperatingSystemAtLeastVersion(sonoma) { return NSMenuItem::sectionHeaderWithTitle(&NSString::from_str(title), mtm); }
    disabled_item(title, mtm)
}

fn disabled_item(title: &str, mtm: MainThreadMarker) -> Retained<NSMenuItem> {
    let item = plain_item(title, mtm);
    item.setEnabled(false);
    item
}

fn modifiers(key: &MenuKey) -> NSEventModifierFlags {
    let mut flags = NSEventModifierFlags::empty();
    for (on, flag) in [(key.command, NSEventModifierFlags::Command), (key.shift, NSEventModifierFlags::Shift), (key.option, NSEventModifierFlags::Option),
        (key.control, NSEventModifierFlags::Control)] {
        if on { flags |= flag; }
    }
    flags
}

/// A template image (macOS tints it for light, dark and tinted menu bars) from its 1× and 2× PNGs.
fn template_image(pngs: [&[u8]; 2]) -> Retained<NSImage> {
    let size = NSSize::new(ICON_POINTS, ICON_POINTS);
    let image = NSImage::initWithSize(NSImage::alloc(), size);
    for png in pngs {
        let Some(rep) = NSBitmapImageRep::imageRepWithData(&NSData::with_bytes(png)) else { continue };
        rep.setSize(size);
        image.addRepresentation(&rep);
    }
    image.setTemplate(true);
    image
}

/// A two-button question. The default button (Return) comes first; `default_goes_ahead` says
/// whether it is the one that does the thing asked about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Alert {
    pub title: String,
    pub message: String,
    pub default_button: &'static str,
    /// Esc picks it when it is titled "Cancel" (AppKit's own rule).
    pub other_button: &'static str,
    pub default_goes_ahead: bool,
}

/// Ask with an app-modal alert, CluelyRS brought forward, and say whether the user went ahead.
/// The alert runs its own event loop, so this must never be called inside a GPUI update: GPUI's
/// main-thread tasks can run meanwhile and would find GPUI already borrowed. From inside one,
/// use `confirm_later`.
pub fn confirm(alert: &Alert) -> bool {
    let Some(mtm) = MainThreadMarker::new() else { return false };
    let panel = NSAlert::new(mtm);
    panel.setMessageText(&NSString::from_str(&alert.title));
    panel.setInformativeText(&NSString::from_str(&alert.message));
    // SAFETY: a valid image, set on the main thread.
    unsafe { panel.setIcon(Some(&super::app_icon())) };
    panel.addButtonWithTitle(&NSString::from_str(alert.default_button));
    panel.addButtonWithTitle(&NSString::from_str(alert.other_button));
    #[allow(deprecated)] // `activate` needs macOS 14.
    NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
    (panel.runModal() == NSAlertFirstButtonReturn) == alert.default_goes_ahead
}

/// `confirm` on the next turn of the main queue, then `confirmed` if the user went ahead. For
/// callers inside a GPUI update: the alert runs after it returns, and while it is up the main
/// queue (where GPUI runs its tasks) waits for it, so nothing re-enters GPUI.
pub fn confirm_later(alert: Alert, confirmed: impl FnOnce() + Send + 'static) {
    DispatchQueue::main().exec_async(move || if confirm(&alert) { confirmed() });
}
