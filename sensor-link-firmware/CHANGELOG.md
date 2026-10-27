# Changelog

## Unreleased

- Fix: firmware download over the Quectel modem retries a slice up to 3 times
  instead of failing on the first slow `CONNECT` or broken read. The `CONNECT`
  timeout of a range request is 15 s (was 5 s). A failed read of the modem file
  no longer counts as the end of the slice, which silently dropped up to 16 KB of
  the image. The client sends `FWStatus::Failed` when a download fails or a chunk
  cannot be decoded.
- The network task logs `logic::network::CONNECTED_MARKER` once the MQTT
  session is up, in place of "Network Connected", so provisioning can verify
  a device's connection over RTT.
- Dispatch barrier: `DispatchBarrier` lets another task ask the dispatch task to
  reply with `Signal::DispatchDrained` once everything it received so far has been
  stored, sent and confirmed by the network task. Breaking: `dispatch_task` takes
  a `&DispatchBarrier` argument.
- `DispatchStore::is_drained` reports whether every stored event and sensor data
  item has been confirmed. Breaking for `DispatchStore` implementations.
- `LatencyControlledSerializer::flush` forces buffered data out, one packet per
  call, without waiting for a timeout. Breaking for implementations.
- The network task sends `Signal::StatusSent` after publishing a status, carrying
  the new `NetworkStatus::confirmation_token`. Breaking for `NetworkStatus`
  implementations.
- Fix: the queue's read index no longer overflows when peeking sequence number
  `u32::MAX`.
- Fix: the dispatch task wakes when the network task confirms or aborts an
  upload, so an aborted upload is retried without waiting for new data. Adds
  `DispatchStore::wait_confirmation`, breaking for implementations.
- Fix: the Quectel MQTT will (ungraceful offline) is no longer retained. The
  stale retained will made the server log "Offline (ungraceful)" on every
  reconnect, even after a graceful offline.
- `Confirmable::map` transforms the inner data and keeps the confirm handle.
- Circular stores ignore blocks with a foreign layout (other object id or
  fragment size, e.g. left by older firmware) instead of reading them as data.
- `Circular::auto_format` decides per circular whether such a block is
  reformatted when the write pointer reaches it. Defaults to `true`, so writes
  no longer stop at a foreign block. Breaking: the `auto_format` argument of
  `initialize_writeable_circular_store` is removed. It was one flag shared by
  all circulars in a `Database`, set by whichever caller initialized last.

## 0.2.0 - 2026-09-04

- ADS124S08: added `RefConfig::rail_monitors` to disable the PGA rail monitors,
  which were hardcoded on. Still on by default. Breaking: `RefConfig` gained a
  field, so struct-literal construction needs updating.

## 0.1.0

- Initial version.
