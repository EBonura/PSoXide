+++
title = "psx-cache"
description = "Fixed-capacity residency and eviction"
[extra]
kind = "Crate guide"
eyebrow = "SDK crate"
+++

Use `psx-cache` to track which resources occupy a fixed set of slots. It manages keys, residency state, recency and pinning; your code owns the actual asset storage.

## How the crate is organized

The single module exposes `SlotCache<V, N, MAX_KEY>` and `SlotState`. Slots move from Empty to Loading to Ready. Synchronous callers use `get_or_insert_with`; asynchronous loaders reserve a slot and mark it ready when the transfer completes.

## Integration notes

Keys are bounded by `MAX_KEY`, and payloads must be `Copy`. Pin the working set before allocating resources that could evict it. Handle a failed reservation when no slot is available. Keep byte allocation and release policy separate from this index.

## Complete host example

```rust
use psx_cache::SlotCache;

fn main() {
    let mut cache = SlotCache::<u32, 2, 8>::new();
    let slot = cache.reserve(3).expect("free slot");
    assert!(cache.is_loading(3));
    cache.mark_ready(slot, 42);
    cache.pin(3);
    assert_eq!(cache.get(3), Some(&42));
    cache.unpin_all();
    cache.evict(3);
    assert!(!cache.contains_ready(3));
}
```

Run as a host binary with a path dependency on `sdk/crates/psx-cache`; it does not touch hardware.

## API, dependencies and source structure

{{<sdk_crate name="psx-cache" />}}
