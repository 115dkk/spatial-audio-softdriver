"""End-to-end check of the live path on a CI runner with VB-CABLE.

Plays a dump with bio-live into VB-CABLE's render endpoint ("Speakers (VB-Audio
Virtual Cable)") while recording its capture endpoint ("CABLE Output"),
then compares the recording with bio-replay's offline render of the same dump.
The live path resamples (ASRC against the cable's clock), so the comparison is
windowed: every 50 ms window of the reference must find itself in the
recording within a few samples of the running offset, with a normalised
correlation of at least --min-correlation. Silence in either signal is not
compared.

    python loopback_check.py --bio-live target/release/bio-live.exe \
        --dump fx/pcm71.biodump --profile fx/profile.toml \
        --reference ref.wav --mode exclusive --summary out.json

Exit status 0 when the recording matches, 1 otherwise. CI only.
"""

import argparse
import json
import subprocess
import sys
import time

import numpy as np
import soundfile as sf
from scipy.signal import correlate, resample_poly

WINDOW_S = 0.05
SEARCH = 64  # samples either side of the running offset


def ncc(a, b):
    a = a - a.mean()
    b = b - b.mean()
    denominator = np.sqrt((a * a).sum() * (b * b).sum())
    return float((a * b).sum() / denominator) if denominator > 0 else 0.0


def compare(reference, ref_rate, captured, rate, min_correlation):
    """Windowed match of `reference` inside `captured` (both frames x 2)."""
    if ref_rate != rate:
        reference = resample_poly(reference, rate, ref_rate, axis=0).astype(np.float32)
    summary = {
        "rate": rate,
        "windows": 0,
        "matched": 0.0,
        "median_correlation": 0.0,
        "worst_correlation": 0.0,
        "captured_peak": float(np.abs(captured).max(initial=0.0)),
    }
    # Coarse alignment of the whole signals, on the mono sum.
    ref_mono = reference.sum(axis=1)
    cap_mono = captured.sum(axis=1)
    if len(cap_mono) < len(ref_mono) or not np.any(cap_mono):
        return summary
    lags = correlate(cap_mono, ref_mono, mode="valid", method="fft")
    offset = int(np.argmax(lags))

    window = int(WINDOW_S * rate)
    scores = []
    for start in range(0, len(reference) - window, window):
        ref = reference[start:start + window]
        if np.sqrt((ref * ref).mean()) < 1e-4:
            continue
        best = -1.0
        best_shift = 0
        for shift in range(-SEARCH, SEARCH + 1):
            at = offset + start + shift
            if at < 0 or at + window > len(captured):
                continue
            cap = captured[at:at + window]
            score = min(ncc(ref[:, 0], cap[:, 0]), ncc(ref[:, 1], cap[:, 1]))
            if score > best:
                best, best_shift = score, shift
        offset += best_shift  # follow the clock drift
        scores.append(best)

    if scores:
        scores = np.array(scores)
        summary.update(
            windows=int(len(scores)),
            matched=float((scores >= min_correlation).mean()),
            median_correlation=float(np.median(scores)),
            worst_correlation=float(scores.min()),
        )
    return summary


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--bio-live", required=True)
    parser.add_argument("--dump", required=True)
    parser.add_argument("--profile", required=True)
    parser.add_argument("--reference", required=True)
    parser.add_argument("--mode", choices=["exclusive", "shared"], required=True)
    parser.add_argument("--render", default="VB-Audio Virtual Cable")
    parser.add_argument("--capture", default="CABLE Output")
    parser.add_argument("--min-correlation", type=float, default=0.95)
    parser.add_argument("--min-matched", type=float, default=0.9)
    parser.add_argument("--summary", required=True)
    args = parser.parse_args()

    # Windows CI only; compare() needs no audio device. soundcard talks to
    # WASAPI directly (shared mode with format conversion), where PortAudio's
    # WASAPI host refused VB-CABLE's capture endpoint.
    import soundcard as sc

    reference, ref_rate = sf.read(args.reference, dtype="float32", always_2d=True)
    microphone = sc.get_microphone(args.capture)
    rate = 48_000
    print(f"capturing {microphone.name} at {rate} Hz", flush=True)

    command = [
        args.bio_live, "play",
        "--from-dump", args.dump,
        "--profile", args.profile,
        "--output", args.render,
    ]
    if args.mode == "shared":
        command.append("--shared")
    chunks = []
    with microphone.recorder(samplerate=rate, channels=2, blocksize=rate // 100) as recorder:
        chunks.append(recorder.record(numframes=rate // 2))
        live = subprocess.Popen(command, stderr=subprocess.PIPE, text=True)
        started = time.monotonic()
        while live.poll() is None:
            if time.monotonic() - started > 120:
                live.kill()
                raise SystemExit("bio-live did not stop within 120 s")
            chunks.append(recorder.record(numframes=rate // 20))
        chunks.append(recorder.record(numframes=rate // 2))
    stderr = live.stderr.read() if live.stderr else ""
    print(stderr, flush=True)
    if live.returncode != 0:
        raise SystemExit(f"bio-live exited with {live.returncode}")

    captured = np.concatenate(chunks).astype(np.float32)
    summary = compare(reference, ref_rate, captured, rate, args.min_correlation)
    summary["mode"] = args.mode
    summary["bio_live"] = stderr.strip().splitlines()[-1:]
    with open(args.summary, "w", encoding="utf-8") as out:
        json.dump(summary, out, indent=2)
    print(json.dumps(summary, indent=2))
    if summary["windows"] == 0 or summary["matched"] < args.min_matched:
        print(f"only {summary['matched']:.1%} of windows matched", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
