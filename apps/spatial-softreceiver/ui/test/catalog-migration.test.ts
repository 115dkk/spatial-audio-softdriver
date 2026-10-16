import { afterEach, describe, expect, it } from 'vitest';
import { en as catalogEn, ko as catalogKo } from '../src/catalog';
import { fixture } from '../src/fixtures';
import { chainErrorEntries, codecName, describeOutputMode } from '../src/format';
import { createMockApi } from '../src/mock';
import { objectMap } from '../src/screens/objectmap';
import { setLanguage } from '../src/text';

const languages = ['ko', 'en'] as const;

afterEach(() => setLanguage('ko'));

describe('catalog caller-owned choices', () => {
  it.each(languages)('selects fallback text only for a positive mix count (%s)', (language) => {
    setLanguage(language);
    const engine = fixture('running', language).engine!;
    const expected = language === 'ko'
      ? ['공유로 대체됨', '공유로 대체됨, 믹스 6채널', '독점', '공유']
      : ['Fell back to shared', 'Fell back to shared, 6-channel mix', 'Exclusive', 'Shared'];
    engine.output_fell_back = true;
    for (const mix of [0, -1, 6]) {
      engine.output_mix_channels = mix;
      expect(describeOutputMode(engine)).toBe(expected[mix > 0 ? 1 : 0]);
    }
    engine.output_fell_back = false;
    engine.output_mode = 'exclusive';
    expect(describeOutputMode(engine)).toBe(expected[2]);
    engine.output_mode = 'shared';
    expect(describeOutputMode(engine)).toBe(expected[3]);
  });

  it.each(languages)('renders zero, one and many sound objects in the actual map (%s)', (language) => {
    setLanguage(language);
    const engine = fixture('running', language).engine!;
    const scene = engine.scene;
    expect(scene.length).toBeGreaterThan(1);
    const map = objectMap(engine.directions, true);
    const expected = language === 'ko'
      ? ['소리를 내는 객체가 없습니다.', '소리 객체 1개', '소리 객체 2개']
      : ['No sound objects are active.', '1 sound object', '2 sound objects'];
    const status = () => map.element.querySelector('.map-status')?.textContent;
    expect(status()).toBe(expected[0]);
    for (const n of [1, 2, 0]) {
      map.update({ ...engine, scene: scene.slice(0, n) });
      expect(status()).toBe(expected[n]);
    }
  });

  it.each(languages)('pads unknown codec numbers outside the catalog (%s)', (language) => {
    setLanguage(language);
    const prefix = language === 'ko' ? '알 수 없는 코덱 ' : 'Unknown codec ';
    for (const [code, hex] of [[0, '0x00000000'], [0x1234, '0x00001234'], [0xe06d802c, '0xe06d802c']] as const) {
      expect(codecName({ other: code })).toBe(prefix + hex);
    }
    expect(chainErrorEntries({ future_kind: 2 })).toEqual([{ kind: 'future_kind', label: 'future_kind', count: 2 }]);
  });
});

describe('mock generated supervisor compositions', () => {
  it.each(languages)('composes architecture names in the same language (%s)', async (language) => {
    const catalog = language === 'ko' ? catalogKo : catalogEn;
    for (const arch of ['arm64', 'other'] as const) {
      const start = fixture('setup-welcome', language);
      start.platform = { os_build: 26100, arch, test_signing: true, secure_boot: 'disabled' };
      const api = createMockApi(start);
      const pc = arch === 'arm64' ? catalog.supervisor.arch_arm64 : catalog.supervisor.arch_other;
      await expect(api.dispatch({ kind: 'begin_setup' })).rejects.toEqual({
        message: catalog.supervisor.architecture_mismatch(catalog.supervisor.arch_x64, pc), next: 'none',
      });
    }
  });

  it.each(languages)('appends the Secure Boot suffix only when enabled (%s)', async (language) => {
    const catalog = language === 'ko' ? catalogKo : catalogEn;
    for (const secure_boot of ['enabled', 'disabled', 'unsupported'] as const) {
      const start = fixture('setup-welcome', language);
      start.platform = { os_build: 26100, arch: 'x64', test_signing: false, secure_boot };
      const api = createMockApi(start);
      await expect(api.dispatch({ kind: 'begin_setup' })).rejects.toEqual({
        message: catalog.supervisor.test_signing_disabled +
          (secure_boot === 'enabled' ? catalog.supervisor.test_signing_secure_boot : ''),
        next: 'reboot',
      });
    }
  });

  it.each(languages)('keeps driver diagnosis branches and parameter substitution (%s)', async (language) => {
    const s = (language === 'ko' ? catalogKo : catalogEn).supervisor;
    const cases = [
      [{ problem_code: 52, service_running: false, exclusive_policy: 'missing' }, s.driver_signature_rejected],
      [{ problem_code: 10, service_running: true, exclusive_policy: 'applied' }, s.driver_problem(10)],
      [{ problem_code: 0, service_running: false, exclusive_policy: 'applied' }, s.driver_service_stopped],
      [{ problem_code: 0, service_running: true, exclusive_policy: 'missing' }, s.exclusive_policy_missing],
    ] as const;
    for (const [value, message] of cases) {
      const start = fixture('setup-welcome', language);
      start.driver = { kind: 'installed', value };
      const api = createMockApi(start);
      await expect(api.dispatch({ kind: 'begin_setup' })).rejects.toEqual({ message, next: 'retry' });
    }
  });

  it.each(languages)('keeps file details, errors, and dynamic notes when the language changes (%s)', async (language) => {
    const s = (language === 'ko' ? catalogKo : catalogEn).supervisor;
    const other = language === 'ko' ? 'en' : 'ko';
    const translated = (other === 'ko' ? catalogKo : catalogEn).supervisor;
    const start = fixture('ready', language);
    start.chosen_output = { id: 'old-id', name: start.outputs[0]!.name };
    const api = createMockApi(start);
    await api.dispatch({ kind: 'enable' });
    expect((await api.snapshot()).notes).toEqual([s.output_id_changed(start.outputs[0]!.name)]);
    await api.dispatch({ kind: 'import_brir', value: 'C:\\measurements\\room' });
    await expect(api.dispatch({ kind: 'import_headphone_fir', value: 'C:\\data\\untouched.flac' })).rejects.toEqual({
      message: s.headphone_fir_read_failed('untouched.flac'), next: 'none',
    });
    await api.dispatch({ kind: 'set_language', value: other });
    const changed = await api.snapshot();
    expect(changed.error?.message).toBe(translated.headphone_fir_read_failed('untouched.flac'));
    expect(changed.notes).toEqual([translated.output_id_changed(start.outputs[0]!.name)]);
    expect(changed.profile.name).toBe('room');
    expect(changed.profile.brir).toBe('C:\\measurements\\room');
  });
});
