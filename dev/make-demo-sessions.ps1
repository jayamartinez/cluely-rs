# Builds a demo sessions folder with synthetic screenshots so the Sessions window can be
# reviewed without recording real meetings. Nothing here comes from a real screen.
#
#   pwsh dev/make-demo-sessions.ps1 -Out "$env:TEMP\cluelyrs-demo"
#   $env:CLUELYRS_DATA_DIR = "$env:TEMP\cluelyrs-demo"; cargo run
param([Parameter(Mandatory)][string]$Out)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

$sessions = Join-Path $Out 'sessions'
if (Test-Path $sessions) { Remove-Item -Recurse -Force $sessions }
New-Item -ItemType Directory -Force $sessions | Out-Null

function Brush($hex) { New-Object System.Drawing.SolidBrush ([System.Drawing.ColorTranslator]::FromHtml($hex)) }
function Shot([string]$path, [string]$kind, [string]$title) {
  $bmp = New-Object System.Drawing.Bitmap 1280, 800
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.SmoothingMode = 'AntiAlias'; $g.TextRenderingHint = 'ClearTypeGridFit'
  $g.FillRectangle((Brush '#1e1f22'), 0, 0, 1280, 800)
  $g.FillRectangle((Brush '#2b2d31'), 0, 0, 1280, 44)
  $g.DrawString($title, (New-Object System.Drawing.Font 'Segoe UI', 14), (Brush '#d4d4d4'), 18, 10)
  switch ($kind) {
    'code' {
      $lines = @('class TokenBucket:', '    def __init__(self, rate, burst):', '        self.rate = rate', '        self.tokens = burst',
        '        self.updated = time.monotonic()', '', '    def allow(self) -> bool:', '        now = time.monotonic()',
        '        self.tokens = min(self.burst, self.tokens + (now - self.updated) * self.rate)', '        self.updated = now',
        '        if self.tokens < 1:', '            return False', '        self.tokens -= 1', '        return True')
      $font = New-Object System.Drawing.Font 'Cascadia Mono', 17
      for ($i = 0; $i -lt $lines.Count; $i++) {
        $g.DrawString(($i + 1).ToString().PadLeft(2), $font, (Brush '#5c6370'), 20, 80 + $i * 34)
        $g.DrawString($lines[$i], $font, (Brush ($(if ($lines[$i] -match 'def|class|return|if') { '#7aaeff' } else { '#d4d4d4' }))), 80, 80 + $i * 34)
      }
    }
    'diagram' {
      $pen = New-Object System.Drawing.Pen ([System.Drawing.ColorTranslator]::FromHtml('#7aaeff')), 3
      $font = New-Object System.Drawing.Font 'Segoe UI', 18
      $boxes = @(@(120, 330, 'Clients'), @(440, 330, 'API gateway'), @(800, 200, 'Redis buckets'), @(800, 460, 'Services'))
      foreach ($b in $boxes) { $g.DrawRectangle($pen, $b[0], $b[1], 230, 110); $g.DrawString($b[2], $font, (Brush '#e6e6e6'), $b[0] + 24, $b[1] + 38) }
      $g.DrawLine($pen, 350, 385, 440, 385); $g.DrawLine($pen, 670, 370, 800, 255); $g.DrawLine($pen, 670, 400, 800, 515)
    }
    'slide' {
      $g.DrawString('Onboarding funnel · Q3', (New-Object System.Drawing.Font 'Segoe UI Semibold', 34), (Brush '#f2efe8'), 90, 110)
      $values = @(100, 72, 54, 41, 37); $labels = @('Visited', 'Signed up', 'Verified', 'First project', 'Invited team')
      for ($i = 0; $i -lt 5; $i++) {
        $g.FillRectangle((Brush '#4c8dff'), 90, 230 + $i * 100, $values[$i] * 9, 60)
        $g.DrawString("$($labels[$i])  $($values[$i])%", (New-Object System.Drawing.Font 'Segoe UI', 18), (Brush '#e6e6e6'), 110, 245 + $i * 100)
      }
    }
  }
  $g.Dispose()
  $codec = [System.Drawing.Imaging.ImageCodecInfo]::GetImageEncoders() | Where-Object MimeType -eq 'image/jpeg'
  $params = New-Object System.Drawing.Imaging.EncoderParameters 1
  $params.Param[0] = New-Object System.Drawing.Imaging.EncoderParameter ([System.Drawing.Imaging.Encoder]::Quality), 82L
  $bmp.Save($path, $codec, $params); $bmp.Dispose()
}

function Write-Session($id, $minutesAgo, $durationMin, $session, $shots) {
  $dir = Join-Path $sessions $id
  New-Item -ItemType Directory -Force (Join-Path $dir 'screenshots') | Out-Null
  $start = [DateTimeOffset]::Now.ToUnixTimeSeconds() - $minutesAgo * 60
  $session.id = $id; $session.startedAt = $start; $session.endedAt = $start + $durationMin * 60
  for ($i = 0; $i -lt $shots.Count; $i++) { Shot (Join-Path $dir ('screenshots/{0:000}.jpg' -f ($i + 1))) $shots[$i][0] $shots[$i][1] }
  $session | ConvertTo-Json -Depth 8 | Set-Content (Join-Path $dir 'session.json') -Encoding utf8
}

function L($ms, $who, $text) { [ordered]@{ atMs = $ms; speaker = $who; text = $text } }
function T($ms, $action, $answer, $shot) { [ordered]@{ atMs = $ms; action = $action; question = ''; answer = $answer; screenshot = $shot } }

Write-Session 'demo-system-design' 90 42 ([ordered]@{
  title = 'System design interview'; model = 'Claude · Sonnet'
  transcript = @(
    (L 4000 'them' 'Thanks for joining. Tell me a bit about what you work on today.'),
    (L 21000 'you' 'I work on the platform team, mostly on our API gateway and internal tooling.'),
    (L 178000 'them' "Let's move to design. How would you build rate limiting for a public API that sees bursty traffic?"),
    (L 186000 'you' 'Sure. First, are limits per user, per API key, or per IP?'),
    (L 194000 'them' 'Per API key, with a global ceiling.'),
    (L 221000 'them' 'Good. What happens when Redis is unavailable?'),
    (L 235000 'you' "I'd fail open for paid tiers with a local in-memory bucket as a fallback, and fail closed for anonymous traffic."),
    (L 580000 'them' 'Can you sketch how the gateway talks to the bucket store?'),
    (L 1025000 'them' 'How would that work across regions?'),
    (L 1048000 'you' 'Each region enforces its own share of the limit and reconciles asynchronously, accepting a small overshoot.'),
    (L 1610000 'them' 'What would you monitor once this is live?'),
    (L 1872000 'you' 'What does on-call look like for the platform team?'),
    (L 1890000 'them' "About one week in six. We'll set up an onsite with the platform team next.")
  )
  turns = @(
    (T 192000 'What do I say?' "I'd use a token bucket per API key. It allows short bursts while holding the average rate steady, and I'd keep the buckets in Redis with an atomic Lua script so every gateway shares one count." '001.jpg'),
    (T 590000 'Assist' 'The diagram shows clients hitting the gateway, which checks a Redis bucket before forwarding. Mention the Lua script keeps check-and-decrement atomic.' '002.jpg'),
    (T 1030000 'What do I say?' 'Give each region a share of the global limit and reconcile counts asynchronously. You trade a small, bounded overshoot for low latency.' '003.jpg'),
    (T 1620000 'Follow-ups' "1. How do you handle clock skew between gateways?`n2. What's the cost of a Redis round trip per request?`n3. How would you expose remaining quota to clients?" $null)
  )
  notes = [ordered]@{
    overviews = [ordered]@{
      brief = 'System design round on API rate limiting: token buckets in Redis, failure modes, then multi-region limits.'
      standard = 'A 42-minute system design round. You scoped the problem with clarifying questions, proposed per-key token buckets in Redis, and handled failure modes well. The multi-region discussion was the weakest stretch: you hedged on consistency before landing on regional limits with async reconciliation.'
      detailed = "A 42-minute system design round with the platform team. After a short intro about your gateway work, they asked you to design rate limiting for a bursty public API. You opened with good clarifying questions (per key vs. per IP) and settled on per-key token buckets stored in Redis, made atomic with a Lua script.`n`nOn failure handling you proposed failing open for paid tiers with a local fallback bucket and failing closed for anonymous traffic, which they responded to well. The multi-region section was less confident: you circled consistency options before choosing regional quotas with asynchronous reconciliation and an accepted overshoot. You closed with a question about on-call, and they mentioned an onsite with the platform team as the next step."
    }
    topics = @(
      [ordered]@{ startMs = 0; endMs = 170000; title = 'Intros and background'; detail = "Your current role, the team's platform migration." },
      [ordered]@{ startMs = 178000; endMs = 1000000; title = 'Rate limiting design'; detail = 'Token bucket per key, Redis with Lua, fail-open for paid tiers.' },
      [ordered]@{ startMs = 1025000; endMs = 1860000; title = 'Multi-region consistency'; detail = 'Regional limits with async reconciliation; trade-off on overshoot.' },
      [ordered]@{ startMs = 1870000; endMs = 2520000; title = 'Your questions'; detail = 'On-call load, team size, next steps: onsite with the platform team.' }
    )
    followUps = @('Read up on sliding-window log vs. token bucket', 'Send thank-you note by Friday')
  }
}) @(@('code', 'rate_limiter.py'), @('diagram', 'Whiteboard · gateway'), @('diagram', 'Whiteboard · regions'))

Write-Session 'demo-weekly-sync' 300 28 ([ordered]@{
  title = 'Weekly sync with Maya'; model = 'ChatGPT · Codex'
  transcript = @((L 5000 'them' 'Quick one today. Where are we on the onboarding fixes?'), (L 16000 'you' 'Verification emails ship Thursday; the invite flow is next week.'),
    (L 410000 'them' 'Can we move the invite flow up?'))
  turns = @((T 420000 'What do I say?' 'Only if we drop the bulk-invite screen from this release. The single-invite path can ship with the verification fix.' '001.jpg'))
}) @(,@('slide', 'Onboarding review'))

Write-Session 'demo-product-review' 1500 55 ([ordered]@{
  title = 'Product review · onboarding'; transcript = @((L 3000 'them' 'Walk us through the funnel numbers.'), (L 30000 'them' 'Why does verification drop so much?'))
  turns = @((T 40000 'Assist' 'Verification loses 18 points; most drop-off is users who never open the email. Suggest a resend prompt and magic-link sign-in.' '001.jpg'))
}) @(,@('slide', 'Onboarding funnel'))

Write-Session 'demo-algorithms' 1700 31 ([ordered]@{
  title = 'Algorithms practice'; transcript = @((L 2000 'you' 'Practicing sliding window problems today.'))
  turns = @((T 60000 'Assist' 'This is a sliding-window problem: expand the right edge, shrink the left while the window is invalid, and track the best length.' '001.jpg'))
}) @(,@('code', 'longest_substring.py'))

"Demo sessions written to $sessions"
