"""End-to-end check of the live path on a CI runner with VB-CABLE.

Plays a dump with bio-live into VB-CABLE's render endpoint ("Speakers (VB-Audio
Virtual Cable)") while recording its capture endpoint ("CABLE Output"),
then compares the recording with bio-replay's offline render of the same dump.
The live path resamples (ASRC against the cable's clock), so the comparison is
windowed: every 50 ms window of the reference must find itself in the
recording within a few samples of the running offset, with a normalised
correlation of at least --min-correlation. A window that misses there is looked
for again within 50 ms, so one jump in the recording (a glitch in the shared
audio engine or the capture) costs the windows around it rather than every
window after it. Silence in either signal is not compared. bio-live's own
report must show no underrun while the dump streamed and no device restart:
discontinuities the engine makes are judged there.

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
RESYNC_S = 0.05  # either side, for a window that misses at the running offset


_numpy_fromstring = np.fromstring


def binary_fromstring(string, dtype=float, count=-1, *, sep=""):
    """numpy.fromstring with the binary mode NumPy 2 removed.

    soundcard 0.4.5 reads each WASAPI capture buffer with
    numpy.fromstring(buffer, dtype='float32') (mediafoundation.py); frombuffer
    does the same, and the copy outlives the released buffer.
    """
    if sep:
        return _numpy_fromstring(string, dtype=dtype, count=count, sep=sep)
    return np.frombuffer(string, dtype=dtype, count=count).copy()


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
        "resyncs": 0,
        "missed": [],
        "captured_peak": float(np.abs(captured).max(initial=0.0)),
    }
    # Coarse alignment of the whole signals, on the mono sum.
    ref_mono = reference.sum(axis=1)
    cap_mono = captured.sum(axis=1)
    if len(cap_mono) < len(ref_mono) or not np.any(cap_mono):
        return summary
    lags = correlate(cap_mono, ref_mono, mode="valid", method="fft")
    offset = int(np.argmax(lags))
    # Where the reference starts in the recording, and where the recording
    # first carries sound (10 ms RMS above -50 dBFS): a gap between the two
    # is audio that never reached the capture.
    summary["offset_s"] = round(offset / rate, 4)
    hop = rate // 100
    loud = np.flatnonzero([np.sqrt((cap_mono[i:i + hop] ** 2).mean()) > 10 ** (-50 / 20)
                           for i in range(0, len(cap_mono) - hop, hop)])
    summary["first_sound_s"] = round(loud[0] * hop / rate, 4) if loud.size else None

    window = int(WINDOW_S * rate)
    resync = int(RESYNC_S * rate)

    def score_at(ref, at):
        if at < 0 or at + window > len(captured):
            return -1.0
        cap = captured[at:at + window]
        return min(ncc(ref[:, 0], cap[:, 0]), ncc(ref[:, 1], cap[:, 1]))

    scores = []
    for index, start in enumerate(range(0, len(reference) - window, window)):
        ref = reference[start:start + window]
        if np.sqrt((ref * ref).mean()) < 1e-4:
            continue
        best = -1.0
        best_shift = 0
        for shift in range(-SEARCH, SEARCH + 1):
            score = score_at(ref, offset + start + shift)
            if score > best:
                best, best_shift = score, shift
        if best < min_correlation:
            # Find the window again further away, on the mono sum.
            lo = max(0, offset + start - resync)
            hi = min(len(captured), offset + start + window + resync)
            if hi - lo > window:
                lags = correlate(cap_mono[lo:hi], ref.sum(axis=1), mode="valid", method="fft")
                at = lo + int(np.argmax(lags))
                score = score_at(ref, at)
                if score > best:
                    best, best_shift = score, at - (offset + start)
                    summary["resyncs"] += 1
        if best < min_correlation:
            summary["missed"].append({"window": index, "correlation": round(best, 4),
                                      "offset": int(offset + best_shift)})
        offset += best_shift  # follow the clock drift, or the jump
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
    if sys.argv[1:2] == ["--keep-alive"]:
        return keep_alive(sys.argv[2])
    parser = argparse.ArgumentParser()
    parser.add_argument("--bio-live", required=True)
    parser.add_argument("--dump", required=True)
    parser.add_argument("--profile", required=True)
    parser.add_argument("--reference", required=True)
    parser.add_argument("--mode", choices=["exclusive", "shared"], required=True)
    parser.add_argument("--render", default="VB-Audio Virtual Cable")
    parser.add_argument("--capture", default="CABLE Output")
    parser.add_argument("--period-ms", type=float, help="exclusive device period for bio-live")
    parser.add_argument("--min-correlation", type=float, default=0.95)
    parser.add_argument("--min-matched", type=float, default=0.9)
    parser.add_argument("--summary", required=True)
    parser.add_argument("--report", help="where bio-live writes its JSON report")
    parser.add_argument("--warm", action="store_true",
                        help="keep the render endpoint running with a silent shared stream")
    args = parser.parse_args()

    # Windows CI only; compare() needs no audio device. soundcard talks to
    # WASAPI directly (shared mode with format conversion), where PortAudio's
    # WASAPI host refused VB-CABLE's capture endpoint.
    np.fromstring = binary_fromstring
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
    if args.period_ms:
        command += ["--period-ms", str(args.period_ms)]
    if args.report:
        command += ["--report", args.report]
    warm = None
    if args.warm:
        # Shared mode only: an exclusive open would be refused next to it.
        warm = subprocess.Popen([sys.executable, __file__, "--keep-alive", args.render])
        time.sleep(1.0)
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
    if warm:
        warm.kill()
    stderr = live.stderr.read() if live.stderr else ""
    print(stderr, flush=True)
    if live.returncode != 0:
        raise SystemExit(f"bio-live exited with {live.returncode}")

    captured = np.concatenate(chunks).astype(np.float32)
    summary = compare(reference, ref_rate, captured, rate, args.min_correlation)
    summary["mode"] = args.mode + (" warm" if args.warm else "")
    summary["bio_live"] = stderr.strip().splitlines()[-1:]
    problems = []
    if args.report:
        with open(args.report, encoding="utf-8") as report:
            figures = json.load(report)
        summary["underruns_while_streaming"] = figures["underruns_while_streaming"]
        summary["output_restarts"] = figures["output_restarts"]
        if figures["underruns_while_streaming"]:
            problems.append(f"{figures['underruns_while_streaming']} underruns while streaming")
        if figures["output_restarts"]:
            problems.append(f"{figures['output_restarts']} device restarts")
        if figures["faults"]:
            problems.append(f"faults: {figures['faults']}")
    if summary["windows"] == 0 or summary["matched"] < args.min_matched:
        problems.append(f"only {summary['matched']:.1%} of windows matched")
    with open(args.summary, "w", encoding="utf-8") as out:
        json.dump(summary, out, indent=2)
    print(json.dumps(summary, indent=2))
    for problem in problems:
        print(problem, file=sys.stderr)
    return 1 if problems else 0


def keep_alive(endpoint):
    """Plays silence on `endpoint` until killed (the --warm helper process)."""
    np.fromstring = binary_fromstring
    import soundcard as sc

    rate = 48_000
    silence = np.zeros((rate // 20, 2), dtype=np.float32)
    with sc.get_speaker(endpoint).player(samplerate=rate, channels=2, blocksize=rate // 100) as player:
        while True:
            player.play(silence)


if __name__ == "__main__":
    sys.exit(main())
