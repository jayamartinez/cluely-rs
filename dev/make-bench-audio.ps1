# Synthesizes benchmark speech with the built-in Windows voice, so Parakeet benchmarks can be
# reproduced on any Windows machine without a private recording. Output: 16 kHz mono 16-bit WAV
# with a pause after each question (where an end-of-utterance should fire).
#
#   powershell -File dev/make-bench-audio.ps1 -Out "$env:TEMP\cluelyrs-bench.wav"
#   cargo run --example parakeet_bench -- "$env:TEMP\cluelyrs-bench.wav" realtime
param([Parameter(Mandatory)][string]$Out)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Speech

$lines = @(
    'So how would you design a distributed cache?',
    'Would you use Redis here, or avoid caching entirely?',
    'Okay. And what about cache invalidation when the database changes?',
    'Walk me through what happens when two writers update the same key.',
    'What is the difference between a mutex and a semaphore?',
    'Can you tell me about a time you had to debug a production outage?'
)

$synth = New-Object System.Speech.Synthesis.SpeechSynthesizer
$synth.Rate = 0
$format = New-Object System.Speech.AudioFormat.SpeechAudioFormatInfo(16000, [System.Speech.AudioFormat.AudioBitsPerSample]::Sixteen, [System.Speech.AudioFormat.AudioChannel]::Mono)
$synth.SetOutputToWaveFile($Out, $format)
$prompt = New-Object System.Speech.Synthesis.PromptBuilder
foreach ($line in $lines) {
    $prompt.AppendText($line)
    $prompt.AppendBreak([TimeSpan]::FromMilliseconds(1500))
}
$synth.Speak($prompt)
$voice = $synth.Voice.Name
$synth.Dispose()
Write-Host "Wrote $Out ($($lines.Count) utterances, voice: $voice)"
