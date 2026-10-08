#!/bin/sh
# macOS counterpart of make-bench-audio.ps1: synthesizes the same benchmark questions with the
# built-in voice. Output: 16 kHz mono 16-bit WAV with a pause after each question (where an
# end-of-utterance should fire). Voices differ from Windows, so compare results within one platform.
#
#   sh dev/make-bench-audio.sh "$TMPDIR/cluelyrs-bench.wav"
#   cargo run --example parakeet_bench -- "$TMPDIR/cluelyrs-bench.wav" realtime
set -eu
out=${1:?usage: make-bench-audio.sh <out.wav>}
pause='[[slnc 1500]]'
say -o "$out" --file-format=WAVE --data-format=LEI16@16000 \
    "So how would you design a distributed cache? $pause" \
    "Would you use Redis here, or avoid caching entirely? $pause" \
    "Okay. And what about cache invalidation when the database changes? $pause" \
    "Walk me through what happens when two writers update the same key. $pause" \
    "What is the difference between a mutex and a semaphore? $pause" \
    "Can you tell me about a time you had to debug a production outage? $pause"
echo "Wrote $out (6 utterances, voice: $(defaults read com.apple.speech.voice.prefs SelectedVoiceName 2>/dev/null || echo system default))"
