"""End-to-end driver ring check through ssr-live and VB-CABLE.

Starts ssr-live on the Spatial SoftReceiver control ring, plays a distinct shared-mode tone
into each of the three endpoints (primary, music, passthrough), captures CABLE
Output, then requires every tone to stand at least 20 dB above nearby spectral
bins. A 20 dB margin is
large enough to reject leakage/noise while allowing BRIR gain and cancellation.
"""

import argparse
import json
import os
import subprocess
import sys
import threading
import time

import numpy as np

from endpoints import enumerate_endpoints, find_speaker
from report_check import PRESETS, problems as report_problems
from ring_stats import ENDPOINT_TONES

TONES = ENDPOINT_TONES


def tone_margin_db(signal, rate, frequency, guard_hz=40.0):
    """Returns (tone dBFS, local-noise dBFS, margin dB) for a mono signal."""
    if len(signal) < 2:
        return -200.0, -200.0, 0.0
    window = np.hanning(len(signal))
    spectrum = np.abs(np.fft.rfft(signal * window)) * 2.0 / window.sum()
    frequencies = np.fft.rfftfreq(len(signal), 1.0 / rate)
    peak = spectrum[np.argmin(np.abs(frequencies - frequency))]
    neighborhood = (np.abs(frequencies - frequency) >= guard_hz) & (
        np.abs(frequencies - frequency) <= 300.0
    )
    noise = np.median(spectrum[neighborhood]) if np.any(neighborhood) else 1e-10
    tone_db = 20.0 * np.log10(max(float(peak), 1e-10))
    noise_db = 20.0 * np.log10(max(float(noise), 1e-10))
    return tone_db, noise_db, tone_db - noise_db


def check_report(report):
    return report_problems(report, PRESETS["driver_e2e"])


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--ssr-live", required=True)
    parser.add_argument("--profile", required=True)
    parser.add_argument("--seconds", type=float, default=8.0)
    parser.add_argument("--render", default="VB-Audio Virtual Cable")
    parser.add_argument("--capture", default="CABLE Output")
    parser.add_argument("--report", required=True)
    parser.add_argument("--summary", required=True)
    parser.add_argument("--min-margin-db", type=float, default=20.0)
    parser.add_argument(
        "--record",
        help="also have ssr-live write the READ stream it captured to this dump, "
        "so a failure can be replayed offline (ssr-replay render)",
    )
    args = parser.parse_args()

    from numpy_compat import binary_fromstring
    np.fromstring = binary_fromstring
    import soundcard as sc

    rows = enumerate_endpoints()
    speakers = {endpoint: find_speaker(sc, endpoint, rows) for endpoint in TONES}
    if not all(speakers.values()):
        raise SystemExit(f"Spatial SoftReceiver endpoints missing: {speakers}")
    microphone = sc.get_microphone(args.capture)
    rate = 48_000
    command = [
        args.ssr_live,
        "play",
        "--source",
        "driver",
        "--output",
        args.render,
        "--profile",
        args.profile,
        "--seconds",
        str(args.seconds),
        "--report",
        args.report,
        "--shared",
    ]
    if args.record:
        command += ["--record", args.record]
    chunks = []
    errors = []

    def play(endpoint):
        try:
            t = np.arange(int((args.seconds - 1.0) * rate)) / rate
            tone = (0.25 * np.sin(2 * np.pi * TONES[endpoint] * t)).astype(np.float32)
            stereo = np.stack([tone, tone], axis=1)
            with speakers[endpoint].player(
                samplerate=rate, channels=2, blocksize=rate // 100
            ) as player:
                player.play(stereo)
        except Exception as error:  # CI must preserve the worker failure in its summary.
            errors.append(f"endpoint {endpoint}: {error}")

    with microphone.recorder(samplerate=rate, channels=2, blocksize=rate // 100) as recorder:
        chunks.append(recorder.record(numframes=rate // 2))
        live = subprocess.Popen(command, stderr=subprocess.PIPE, text=True)
        time.sleep(0.5)
        players = [threading.Thread(target=play, args=(endpoint,)) for endpoint in TONES]
        for player in players:
            player.start()
        deadline = time.monotonic() + args.seconds + 30.0
        while live.poll() is None and time.monotonic() < deadline:
            chunks.append(recorder.record(numframes=rate // 20))
        if live.poll() is None:
            live.kill()
            errors.append("ssr-live timed out")
        for player in players:
            player.join()
        chunks.append(recorder.record(numframes=rate // 2))

    stderr = live.stderr.read() if live.stderr else ""
    if live.returncode:
        errors.append(f"ssr-live exited with {live.returncode}: {stderr}")
    captured = np.concatenate(chunks).astype(np.float32)
    mono = captured.mean(axis=1)
    tones = {}
    for endpoint, frequency in TONES.items():
        level, noise, margin = tone_margin_db(mono, rate, frequency)
        tones[str(endpoint)] = {
            "frequency_hz": frequency,
            "tone_dbfs": round(level, 2),
            "noise_dbfs": round(noise, 2),
            "margin_db": round(margin, 2),
        }
        if margin < args.min_margin_db:
            errors.append(f"{frequency:.0f} Hz margin is {margin:.1f} dB")

    report = {}
    if os.path.exists(args.report):
        with open(args.report, encoding="utf-8") as source:
            report = json.load(source)
        errors.extend(check_report(report))
    else:
        errors.append("ssr-live did not write its report")
    summary = {
        "render_endpoints": rows,
        "tones": tones,
        "clock_strategy": report.get("clock_strategy"),
        "report": report,
        "problems": errors,
    }
    with open(args.summary, "w", encoding="utf-8") as output:
        json.dump(summary, output, indent=2)
    print(json.dumps(summary, indent=2))
    step = os.environ.get("GITHUB_STEP_SUMMARY")
    if step:
        with open(step, "a", encoding="utf-8") as output:
            output.write("\n### Driver ring to VB-CABLE\n\n")
            output.write(f"Clock strategy: {summary['clock_strategy']}\n\n")
            for tone in tones.values():
                output.write(
                    f"- {tone['frequency_hz']:.0f} Hz: {tone['tone_dbfs']} dBFS, "
                    f"{tone['margin_db']} dB above local noise\n"
                )
            output.write("\n" + ("FAIL: " + "; ".join(errors) if errors else "ok") + "\n")
    for error in errors:
        print(error, file=sys.stderr)
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
