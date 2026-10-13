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
ANCHOR_S = 0.25  # of the reference's start, correlated to refine the alignment


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


def envelope(mono, hop):
    """RMS of each whole `hop` of samples."""
    n = len(mono) // hop
    return np.sqrt((mono[:n * hop].reshape(n, hop) ** 2).mean(axis=1))


def first_sound(mono, rate):
    """First sample of the first 10 ms whose RMS is above -50 dBFS, or None."""
    hop = rate // 100
    for start in range(0, len(mono) - hop, hop):
        if np.sqrt((mono[start:start + hop] ** 2).mean()) > 10 ** (-50 / 20):
            return start
    return None


def report_problems(figures, require_driver=False, expected_mode=None):
    """Returns failures for missing report state and live-path losses.

    `expected_mode` ("exclusive" or "shared") is the mode the run asked for;
    the report's `output_mode` is the mode the device was actually opened in.
    """
    problems = []
    for key in (
        "underruns_while_streaming",
        "output_restarts",
        "output_reconnects",
        "output_cadence_faults",
        "output_mode",
        "output_fell_back",
        "dropped_frames",
        "lane_underrun_frames",
        "output_overflow_frames",
        "dropped_events",
        "faults",
        "endpoints",
    ):
        if key not in figures:
            problems.append(f"report is missing {key}")

    for key in (
        "underruns_while_streaming",
        "output_restarts",
        "output_reconnects",
        "output_cadence_faults",
        "dropped_frames",
        "lane_underrun_frames",
        "output_overflow_frames",
        "dropped_events",
    ):
        if key in figures and figures[key] != 0:
            problems.append(f"{key} is {figures[key]}")
    if figures.get("output_fell_back"):
        problems.append("exclusive output fell back to shared mode")
    if expected_mode is not None and "output_mode" in figures \
            and figures["output_mode"] != expected_mode:
        problems.append(f"output opened {figures['output_mode']}, expected {expected_mode}")
    if figures.get("faults"):
        problems.append(f"faults: {figures['faults']}")

    endpoints = figures.get("endpoints")
    if not isinstance(endpoints, list):
        problems.append("report endpoints must be an array")
    else:
        if len(endpoints) != 4:
            problems.append("report endpoints must contain exactly four entries")
        for expected, endpoint in enumerate(endpoints):
            if not isinstance(endpoint, dict):
                problems.append(f"report endpoint {expected} must be an object")
                continue
            for key in ("endpoint", "format", "epoch", "unsupported"):
                if key not in endpoint:
                    problems.append(f"report endpoint {expected} is missing {key}")
            if endpoint.get("endpoint") != expected:
                problems.append(
                    f"report endpoint slot {expected} names {endpoint.get('endpoint')}"
                )
            if endpoint.get("unsupported") is not None:
                problems.append(
                    f"endpoint {expected} has unsupported format: {endpoint['unsupported']}"
                )

    if require_driver:
        driver = figures.get("driver")
        if driver is None:
            problems.append("report is missing driver statistics")
        elif not isinstance(driver.get("endpoints"), list):
            problems.append("driver statistics are missing endpoints")
        else:
            for endpoint in driver["endpoints"]:
                if "lost_bytes" not in endpoint:
                    problems.append(
                        f"driver endpoint {endpoint.get('endpoint')} is missing lost_bytes"
                    )
                elif endpoint["lost_bytes"] != 0:
                    problems.append(
                        f"driver endpoint {endpoint.get('endpoint')} lost "
                        f"{endpoint['lost_bytes']} bytes"
                    )
    return problems


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
    # Coarse alignment on the mono sums, in two steps. The 10 ms RMS envelopes
    # find where the reference's sound lies in the recording; a clock drift, a
    # nearly periodic signal or a short hole hardly moves that. The first
    # ANCHOR_S of the reference then refines it within one envelope step either
    # side. Correlating the whole waveforms instead failed once the live clock
    # drifted: the fixtures are nearly periodic, the peak smeared out, and at
    # -609 ppm the argmax fell inside the recording's leading silence (0.003 s
    # against sound at 0.67 s).
    ref_mono = reference.sum(axis=1)
    cap_mono = captured.sum(axis=1)
    if len(cap_mono) < len(ref_mono) or not np.any(cap_mono):
        return summary
    hop = rate // 100
    ref_env, cap_env = envelope(ref_mono, hop), envelope(cap_mono, hop)
    coarse = int(np.argmax(correlate(cap_env, ref_env, mode="valid", method="fft"))) * hop
    anchor = ref_mono[:int(ANCHOR_S * rate)]
    lo = max(0, coarse - 2 * hop)
    hi = min(len(cap_mono), coarse + 2 * hop + len(anchor))
    offset = lo + int(np.argmax(correlate(cap_mono[lo:hi], anchor, mode="valid", method="fft")))
    cap_onset = first_sound(cap_mono, rate)
    # Where the reference starts in the recording, and where the recording
    # first carries sound: a gap between the two is audio that never reached
    # the capture.
    summary["offset_s"] = round(offset / rate, 4)
    summary["first_sound_s"] = round(cap_onset / rate, 4) if cap_onset is not None else None

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
        summary["underruns_while_streaming"] = figures.get("underruns_while_streaming")
        summary["output_restarts"] = figures.get("output_restarts")
        problems.extend(report_problems(figures, expected_mode=args.mode))
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
