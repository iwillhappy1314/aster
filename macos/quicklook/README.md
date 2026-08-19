# Aster Quick Look

Aster ships a Finder Quick Look preview extension for Markdown files. The extension uses a small Swift `WKWebView` shell, but Markdown semantics stay in Rust:

```text
Markdown
  -> gpui-gfm parser / shared IR
     -> GPUI renderer (Aster)
     -> HTML renderer (Quick Look)
        -> C ABI
        -> WKWebView
```

The `gpui-gfm` dependency is built with `default-features = false, features = ["html"]` for Quick Look, so the extension does not link GPUI or Metal.

## Build the extension

```bash
./scripts/build-quicklook.sh arm64
./scripts/build-quicklook.sh x86_64
```

The resulting bundle is written under `target/quicklook/<arch>/AsterQuickLook.appex`.

`./scripts/build-dmg.sh` builds and embeds the matching `.appex` automatically, including both architectures for a universal build.

## Signing

Local builds use ad-hoc signing by default. To sign with an Apple identity:

```bash
ASTER_CODESIGN_IDENTITY="Developer ID Application: Example (TEAMID)" \
  ./scripts/build-dmg.sh arm64
```

## Local images

Quick Look gives the extension direct access to the selected Markdown file, but Markdown commonly references sibling assets. The current extension therefore carries a read-only temporary file exception so relative images work from the preview. This is appropriate for the current direct-distribution build; a future Mac App Store build should replace it with a narrower attachment/security-scoped asset strategy.

## Refresh during development

After replacing a locally installed build, Finder may keep the previous extension process alive. These commands are useful while testing:

```bash
killall quicklookd 2>/dev/null || true
killall Finder
```
