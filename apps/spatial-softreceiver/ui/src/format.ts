// Formats and numbers as the words the screen shows. No judgement here: the
// screen only names what the snapshot says.

import type {
  BitstreamCodec,
  BitstreamFormat,
  ChainErrorMap,
  EngineStatus,
  PcmFormat,
  SourceFormat,
  UnsupportedReason,
} from './bindings';
import { text } from './text';

/** U+00A0: keeps a number on the same line as its unit. */
export const NBSP = '\u00a0';

/**
 * 48000 → "48 kHz", 44100 → "44.1 kHz". A number and its unit are joined
 * by a no-break space (NBSP) so a narrow card never splits them.
 */
export function describeRate(hz: number): string {
  const khz = hz / 1000;
  return `${Number.isInteger(khz) ? khz.toFixed(0) : khz.toFixed(1)}${NBSP}kHz`;
}

/** float/32 → "float32", int/24 in 32 → "int24/32". */
export function describeSamples(format: PcmFormat): string {
  if (format.sample_type === 'float') return `float${format.bits_per_sample}`;
  return format.valid_bits === format.bits_per_sample || format.valid_bits === 0
    ? `int${format.bits_per_sample}`
    : `int${format.valid_bits}/${format.bits_per_sample}`;
}

export function codecName(codec: BitstreamCodec): string {
  return typeof codec === 'string'
    ? text.codecs[codec]
    : text.format.otherCodec('0x' + codec.other.toString(16).padStart(8, '0'));
}

function describePcm(format: PcmFormat): string {
  const kind = format.channel_mask === 0 ? text.format.direct : text.format.pcm;
  return [kind, text.format.channels(format.channels), describeRate(format.sample_rate), describeSamples(format)].join(
    ' ',
  );
}

/** The codec with the channels and rate it encodes (0 means not given). */
function describeBitstream(format: BitstreamFormat): string {
  const parts = [codecName(format.codec)];
  if (format.encoded_channels > 0) parts.push(text.format.channels(format.encoded_channels));
  if (format.encoded_rate > 0) parts.push(describeRate(format.encoded_rate));
  return parts.join(' ');
}

/**
 * The incoming format of an entrance, or "입력 없음": "PCM 8채널 48 kHz
 * float32", "직접 출력 12채널 96 kHz int24" (mask 0), "DD+ 6채널 48 kHz".
 */
export function describeFormat(format: SourceFormat | null): string {
  if (format === null) return text.status.noInput;
  return format.kind === 'pcm' ? describePcm(format.value) : describeBitstream(format.value);
}

/** The IEC 61937 container a bitstream rides in: "전송 2채널 192 kHz". */
export function describeCarrier(format: SourceFormat | null): string | null {
  if (format === null || format.kind !== 'bitstream') return null;
  const { container_channels, container_rate } = format.value;
  return `${text.format.container} ${text.format.channels(container_channels)} ${describeRate(container_rate)}`;
}

export function describeUnsupported(reason: UnsupportedReason): string {
  return text.unsupported[reason];
}

/** One decimal, with a real minus sign kept for negative values. */
export function oneDecimal(value: number): string {
  const rounded = Math.round(value * 10) / 10;
  return (Object.is(rounded, -0) ? 0 : rounded).toFixed(1);
}

export function describeVolume(volumeDb: number, muted: boolean): string {
  const db = `${oneDecimal(volumeDb)}${NBSP}dB`;
  return muted ? `${text.status.muted} (${db})` : db;
}

/** Whole counts with digit grouping: 1204 → "1,204". */
export function count(value: number): string {
  return Math.round(value).toLocaleString('en-US');
}

/** The chain errors that happened, by kind, largest first. */
export function chainErrorEntries(map: ChainErrorMap): { kind: string; label: string; count: number }[] {
  return Object.entries(map)
    .filter((entry): entry is [string, number] => typeof entry[1] === 'number' && entry[1] > 0)
    .map(([kind, n]) => ({ kind, label: text.chainErrorKinds[kind] ?? kind, count: n }))
    .sort((a, b) => b.count - a.count || a.kind.localeCompare(b.kind));
}

export function chainErrorTotal(map: ChainErrorMap): number {
  return Object.values(map).reduce<number>((sum, n) => sum + (n ?? 0), 0);
}

export function describeOutputMode(engine: EngineStatus): string {
  if (engine.output_fell_back) {
    return engine.output_mix_channels > 0
      ? text.meters.fellBackMix(engine.output_mix_channels)
      : text.meters.fellBack;
  }
  return engine.output_mode === 'exclusive' ? text.meters.exclusive : text.meters.shared;
}

export function describeOutputFormat(engine: EngineStatus): string {
  if (engine.output_rate === 0) return '-';
  return `${describeRate(engine.output_rate)} · ${text.format.channels(engine.output_channels)}`;
}

export function describeClock(engine: EngineStatus): string {
  const name = engine.clock === 'trim' ? text.meters.trim : text.meters.asrc;
  return `${name} · ${oneDecimal(engine.clock_ppm)}${NBSP}${text.meters.ppm}`;
}
