//! Build-validated, embedded Glasgow revC UART resources.

/// One revision-specific embedded gateware resource.
#[derive(Clone, Copy, Debug)]
pub(super) struct GlasgowResource {
    /// Exact board stepping, `C0` through `C3`.
    pub revision: &'static str,
    /// Raw FPGA bitstream.
    pub bitstream: &'static [u8],
    /// UTF-8 JSON manifest that describes this bitstream and its API-9 allocations.
    pub manifest: &'static [u8],
}

include!(concat!(env!("OUT_DIR"), "/glasgow_resources.rs"));

/// Select a resource only for an explicitly supported revC stepping.
#[must_use]
pub(super) fn for_revision(revision: &str) -> Option<&'static GlasgowResource> {
    GLASGOW_RESOURCES
        .iter()
        .find(|resource| resource.revision == revision)
}
