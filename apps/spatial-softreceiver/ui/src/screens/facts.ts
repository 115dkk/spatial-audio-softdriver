// Platform and driver facts as rows, shared by the wizard and settings.

import type { DriverState, Platform } from '../bindings';
import type { Child } from '../dom';
import { text } from '../text';

export function platformRows(platform: Platform): [string, Child][] {
  const f = text.facts;
  return [
    [f.osBuild, String(platform.os_build)],
    [f.arch, f.archNames[platform.arch]],
    [f.testSigning, platform.test_signing ? f.on : f.off],
    [f.secureBoot, f.secureBootNames[platform.secure_boot]],
  ];
}

export function driverRows(driver: DriverState): [string, Child][] {
  const f = text.facts;
  switch (driver.kind) {
    case 'not_installed':
      return [[f.driverState, f.driverNotInstalled]];
    case 'reboot_required':
      return [[f.driverState, f.driverRebootRequired]];
    case 'installed': {
      const { problem_code, service_running, exclusive_policy } = driver.value;
      return [
        [f.driverState, f.driverInstalled],
        [f.problemCode, problem_code === 0 ? f.problemNone : String(problem_code)],
        [f.service, service_running ? f.serviceRunning : f.serviceStopped],
        [f.exclusivePolicy, f.policyNames[exclusive_policy]],
      ];
    }
  }
}
