# Changelog

## Unreleased

- Dispatch barrier: `DispatchBarrier` lets another task ask the dispatch task to
  reply with `Signal::DispatchDrained` once everything it received so far has been
  stored, sent and confirmed by the network task. Breaking: `dispatch_task` takes
  a `&DispatchBarrier` argument.
- `DispatchStore::is_drained` reports whether every stored event and sensor data
  item has been confirmed. Breaking for `DispatchStore` implementations.
- `LatencyControlledSerializer::flush`/`is_flushing` force buffered data out
  without waiting for a timeout. Breaking for implementations.
- The network task sends `Signal::StatusSent` after publishing a status, carrying
  the new `NetworkStatus::confirmation_token`. Breaking for `NetworkStatus`
  implementations.
- Fix: the queue's read index no longer overflows when peeking sequence number
  `u32::MAX`.

## 0.2.0 - 2026-09-04

- ADS124S08: added `RefConfig::rail_monitors` to disable the PGA rail monitors,
  which were hardcoded on. Still on by default. Breaking: `RefConfig` gained a
  field, so struct-literal construction needs updating.

## 0.1.0

- Initial version.
