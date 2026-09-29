// naive-ui ships each locale's declaration as a sibling `.d.ts`, which
// TypeScript does not pair with `.mjs` imports automatically. These wildcard
// declarations type the direct per-locale module paths used by src/i18n.
declare module "naive-ui/es/date-picker/index.mjs" {
  export { NDatePicker } from "naive-ui";
}

declare module "naive-ui/es/locales/common/*.mjs" {
  import type { NLocale } from "naive-ui";
  const locale: NLocale;
  export default locale;
}

declare module "naive-ui/es/locales/date/*.mjs" {
  import type { NDateLocale } from "naive-ui";
  const dateLocale: NDateLocale;
  export default dateLocale;
}
