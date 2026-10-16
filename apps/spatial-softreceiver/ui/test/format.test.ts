import { describe, expect, it } from 'vitest';
import type { SourceFormat } from '../src/bindings';
import {
  chainErrorEntries,
  chainErrorTotal,
  codecName,
  describeCarrier,
  describeFormat,
  describeRate,
  describeUnsupported,
  describeVolume,
  oneDecimal,
} from '../src/format';

const pcm = (channels: number, rate: number, mask: number, type: 'int' | 'float', bits: number, valid = bits) =>
  ({
    kind: 'pcm',
    value: { channels, sample_rate: rate, bits_per_sample: bits, valid_bits: valid, sample_type: type, channel_mask: mask },
  }) satisfies SourceFormat;

describe('describeFormat', () => {
  it('names PCM with a channel mask', () => {
    expect(describeFormat(pcm(8, 48000, 1599, 'float', 32))).toBe('PCM 8채널 48\u00a0kHz float32');
  });

  it('names mask 0 as direct output', () => {
    expect(describeFormat(pcm(12, 96000, 0, 'int', 24))).toBe('직접 출력 12채널 96\u00a0kHz int24');
  });

  it('shows integer samples padded in a wider container', () => {
    expect(describeFormat(pcm(2, 44100, 3, 'int', 32, 24))).toBe('PCM 2채널 44.1\u00a0kHz int24/32');
  });

  it('names a bitstream by codec and what it encodes', () => {
    const format: SourceFormat = {
      kind: 'bitstream',
      value: { codec: 'eac3_joc', container_rate: 192000, container_channels: 2, encoded_rate: 48000, encoded_channels: 6 },
    };
    expect(describeFormat(format)).toBe('DD+ JOC 6채널 48\u00a0kHz');
    expect(describeCarrier(format)).toBe('전송 2채널 192\u00a0kHz');
  });

  it('leaves out encoded values the stream did not give', () => {
    const format: SourceFormat = {
      kind: 'bitstream',
      value: { codec: 'mat21', container_rate: 192000, container_channels: 8, encoded_rate: 0, encoded_channels: 0 },
    };
    expect(describeFormat(format)).toBe('MAT 2.1');
  });

  it('says when nothing comes in', () => {
    expect(describeFormat(null)).toBe('입력 없음');
    expect(describeCarrier(null)).toBeNull();
    expect(describeCarrier(pcm(2, 48000, 3, 'int', 16))).toBeNull();
  });
});

describe('names and numbers', () => {
  it('names codecs by format name only', () => {
    expect(codecName('true_hd')).toBe('TrueHD');
    expect(codecName('ac3')).toBe('AC-3');
    expect(codecName('dts')).toBe('DTS');
    expect(codecName({ other: 0xe06d802c })).toBe('알 수 없는 코덱 0xe06d802c');
  });

  it('turns unsupported reasons into sentences', () => {
    expect(describeUnsupported('format')).toBe('지원하지 않는 형식입니다.');
    expect(describeUnsupported('source_rate')).toBe('48 kHz로 바꿀 수 없는 표본율입니다.');
    expect(describeUnsupported('direct_layout')).toBe('이 채널 수에는 직접 배치가 없습니다.');
  });

  it('rounds to one decimal without a negative zero', () => {
    expect(oneDecimal(12.44)).toBe('12.4');
    expect(oneDecimal(-37.5)).toBe('-37.5');
    expect(oneDecimal(-0.04)).toBe('0.0');
    expect(describeRate(48000)).toBe('48\u00a0kHz');
  });

  it('shows volume and mute', () => {
    expect(describeVolume(-6, false)).toBe('-6.0\u00a0dB');
    expect(describeVolume(0, true)).toBe('음소거 (0.0\u00a0dB)');
  });

  it('lists chain errors that happened, largest first', () => {
    const map = { corrupt_payload: 2, missing_direct_layout: 41, decoder_behind: 0, future_kind: 1 };
    expect(chainErrorTotal(map)).toBe(44);
    expect(chainErrorEntries(map)).toEqual([
      { kind: 'missing_direct_layout', label: '직접 배치 없음', count: 41 },
      { kind: 'corrupt_payload', label: '깨진 데이터', count: 2 },
      { kind: 'future_kind', label: 'future_kind', count: 1 },
    ]);
  });
});
