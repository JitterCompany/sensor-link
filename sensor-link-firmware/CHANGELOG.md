# Changelog

## 0.2.0 - 2026-09-04

- ADS124S08: added `RefConfig::rail_monitors` to disable the PGA rail monitors,
  which were hardcoded on. Still on by default. Breaking: `RefConfig` gained a
  field, so struct-literal construction needs updating.

## 0.1.0

- Initial version.
