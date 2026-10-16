// Screen text is a projection of the generated catalog. Sentences from Rust
// (UserError, notes) are shown as they come, not translated by the screen.

import type { Language } from './bindings';
import { en as catalogEn, ko as catalogKo, type Catalog } from './catalog';

type Screen = Catalog['screen'];
type Common = Catalog['common'];
type Text = Omit<Screen, 'endpoints' | 'chainErrorKinds' | 'status' | 'profile'> & {
  product: Catalog['names']['product'];
  endpoints: (Screen['endpoints'][number] & { name: string })[];
  chainErrorKinds: Screen['chainErrorKinds'] & Record<string, string | undefined>;
  status: Screen['status'] & { powerOn: Common['turn_on']; powerOff: Common['turn_off'] };
  profile: Screen['profile'] & {
    fromFile: Common['profile_file_takes_precedence'];
    testOff: Common['test_after_enable'];
  };
};

function project(catalog: Catalog): Text {
  const { screen, names, common } = catalog;
  return {
    ...screen,
    product: names.product,
    // Sentences the supervisor also says live once in [common].
    status: { ...screen.status, powerOn: common.turn_on, powerOff: common.turn_off },
    profile: {
      ...screen.profile,
      fromFile: common.profile_file_takes_precedence,
      testOff: common.test_after_enable,
    },
    endpoints: [
      { ...screen.endpoints[0]!, name: names.endpoint_main },
      { ...screen.endpoints[1]!, name: names.endpoint_music },
      { ...screen.endpoints[2]!, name: names.endpoint_pass },
    ],
  };
}

export const ko = project(catalogKo);
export const en = project(catalogEn);

/** Korean remains the default for the gallery and callers without a snapshot. */
export let text: Text = ko;

/** Called with the supervisor's resolved language, never the user's override. */
export function setLanguage(language: Language): void {
  text = language === 'en' ? en : ko;
}
