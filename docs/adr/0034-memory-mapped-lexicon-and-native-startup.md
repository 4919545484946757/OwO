# ADR 0034: Memory-mapped lexicon and native startup

## Status

Accepted for the Windows 11 alpha runtime.

## Decision

- OWLX v2 stores compact entries, integer syllable IDs, the syllable pool,
  initial indexes and mixed-abbreviation indexes as aligned immutable sections.
- Windows loads v2 through `CreateFileMappingW` and `MapViewOfFile`; lookups
  materialize only returned candidates.
- The installer verifies the copied lexicon with SHA-256 and writes a validation
  record containing the format version, file size and NTFS write timestamp.
  Matching installations skip the full payload checksum. Uncached development
  files still verify the embedded FNV-1a payload checksum.
- `owo_runtime_launcher.exe` replaces the PowerShell/CIM sign-in path. It starts
  Core and ModelHost independently, then waits on a Windows event for Core only.
  libime cold loading therefore cannot block basic candidates.
- Core and ModelHost emit JSON startup phase durations into their normal logs.

## Compatibility

The loader retains OWLX v1 read compatibility so existing component dictionaries
can be merged into v2. Release and development runtime paths select the generated
v2 aggregate dictionary.
