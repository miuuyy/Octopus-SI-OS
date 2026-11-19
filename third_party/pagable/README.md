# pagable 0.4.1

The Rust sources and Apache-2.0 license come from the published pagable
0.4.1 crate (archive SHA-256
`3658968938a4d1eaa1987e69dcd84b01fb067c5b3416dccc8d71373b6ded6821`).
[Upstream source](https://github.com/facebook/starlark-rust/tree/8978f3cd91b2908e1e61f6b58f84293097abf8f1/pagable).

Octopus builds postcard with the `use-crc` and `use-std` features required
by this crate. Disabling postcard’s default embedded feature removes
`heapless` 0.7 and `atomic-polyfill` from the resolved graph. A compatible
upstream release with this feature selection can replace the local copy.
