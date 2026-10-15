# dmgbuild settings for the CluelyRS drag-to-Applications DMG; scripts/bundle-macos.sh runs
#   dmgbuild -s packaging/macos/dmg_settings.py -D app=<CluelyRS.app> -D icon=<.icns> \
#            -D background=<.tiff> "<volume name>" <out.dmg>
# dmgbuild writes the .DS_Store (window layout) itself, so no Finder or AppleScript is involved.
# `defines` is provided by dmgbuild from the -D options.
import os.path

app = defines["app"]  # noqa: F821
app_name = os.path.basename(app.rstrip("/"))

# UDZO (zlib) opens on every macOS we support (13+); ULFO/ULMO save little on a single binary.
format = "UDZO"
filesystem = "HFS+"

files = [app]
symlinks = {"Applications": "/Applications"}
icon = defines["icon"]  # noqa: F821  volume icon, the app's own

# The window matches the 640 x 400 pt background (packaging/macos/dmg-background.svg). The icon
# centres must stay on the ends of the arrow drawn there.
background = defines["background"]  # noqa: F821
window_rect = ((200, 120), (640, 400))
icon_locations = {
    app_name: (160, 176),
    "Applications": (480, 176),
}

default_view = "icon-view"
show_status_bar = False
show_tab_view = False
show_toolbar = False
show_pathbar = False
show_sidebar = False
show_icon_preview = False
show_item_info = False
arrange_by = None
label_pos = "bottom"
icon_size = 128
text_size = 13
