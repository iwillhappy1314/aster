import Cocoa
import Quartz
import WebKit
import AsterMarkdownBridge

private enum PreviewError: Error {
    case renderFailed
}

@objc(AsterQuickLookPreviewViewController)
final class PreviewViewController: NSViewController, QLPreviewingController, WKNavigationDelegate {
    private let webView: WKWebView

    override init(nibName nibNameOrNil: NSNib.Name?, bundle nibBundleOrNil: Bundle?) {
        let configuration = WKWebViewConfiguration()
        configuration.defaultWebpagePreferences.allowsContentJavaScript = false
        webView = WKWebView(frame: .zero, configuration: configuration)
        super.init(nibName: nibNameOrNil, bundle: nibBundleOrNil)
    }

    required init?(coder: NSCoder) {
        let configuration = WKWebViewConfiguration()
        configuration.defaultWebpagePreferences.allowsContentJavaScript = false
        webView = WKWebView(frame: .zero, configuration: configuration)
        super.init(coder: coder)
    }

    override func loadView() {
        view = NSView(frame: NSRect(x: 0, y: 0, width: 900, height: 700))
        webView.frame = view.bounds
        webView.autoresizingMask = [.width, .height]
        webView.navigationDelegate = self
        view.addSubview(webView)
    }

    func preparePreviewOfFile(at url: URL) async throws {
        let markdown = try String(contentsOf: url, encoding: .utf8)
        let html: String = try markdown.withCString { source in
            guard let rendered = aster_markdown_to_html(source) else {
                throw PreviewError.renderFailed
            }
            defer { aster_string_free(rendered) }
            return String(cString: rendered)
        }

        // Using the Markdown file's directory as the base URL preserves relative
        // links and local images in the same way Aster resolves document assets.
        webView.loadHTMLString(html, baseURL: url.deletingLastPathComponent())
    }

    func webView(
        _ webView: WKWebView,
        decidePolicyFor navigationAction: WKNavigationAction,
        decisionHandler: @escaping (WKNavigationActionPolicy) -> Void
    ) {
        guard navigationAction.navigationType == .linkActivated,
              let url = navigationAction.request.url,
              let scheme = url.scheme?.lowercased(),
              ["http", "https", "mailto"].contains(scheme)
        else {
            decisionHandler(.allow)
            return
        }

        NSWorkspace.shared.open(url)
        decisionHandler(.cancel)
    }
}
