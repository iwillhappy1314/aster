# Aster Quick Look

Aster ships a Finder Quick Look preview extension for Markdown files. Markdown semantics stay in Rust and Finder receives HTML through Quick Look's data-based preview API:

```text
Markdown
  -> gpui-gfm parser / shared IR
     -> GPUI renderer (Aster)
     -> HTML renderer (Quick Look)
        -> C ABI
        -> QLPreviewProvider / QLPreviewReply(.html)
        -> Finder Quick Look
```

The `gpui-gfm` dependency is built with `default-features = false, features = ["html"]` for Quick Look, so the extension does not link GPUI or Metal.

The data-based Quick Look API used by the extension requires macOS 12 or newer. This only changes the Quick Look extension deployment target; the containing Aster app keeps its own deployment configuration.

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

The initial data-based preview returns self-contained HTML. Local relative Markdown images are not attached yet. They should be implemented with `QLPreviewReplyAttachment` and `cid:` references rather than broad filesystem access.

## Refresh during development

After replacing a locally installed build, Finder may keep the previous extension process alive. These commands are useful while testing:

```bash
killall quicklookd 2>/dev/null || true
killall Finder
```
