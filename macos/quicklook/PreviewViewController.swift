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
    private var previewHTMLURL: URL?

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

    deinit {
        removeTemporaryPreview()
    }

    override func loadView() {
        NSLog("AsterQuickLook: loadView")
        view = NSView(frame: NSRect(x: 0, y: 0, width: 900, height: 700))
        webView.frame = view.bounds
        webView.autoresizingMask = [.width, .height]
        webView.navigationDelegate = self
        view.addSubview(webView)
    }

    // In current macOS SDKs the completion-handler API is imported into Swift
    // as this async protocol requirement. Defining both forms creates the same
    // Objective-C selector twice, so keep only the async implementation.
    func preparePreviewOfFile(at url: URL) async throws {
        NSLog("AsterQuickLook: preparing %@", url.path)
        try preparePreview(at: url)
    }

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        NSLog("AsterQuickLook: WebView finished loading")
    }

    func webView(
        _ webView: WKWebView,
        didFail navigation: WKNavigation!,
        withError error: Error
    ) {
        NSLog("AsterQuickLook: WebView load failed: %@", String(describing: error))
    }

    func webView(
        _ webView: WKWebView,
        didFailProvisionalNavigation navigation: WKNavigation!,
        withError error: Error
    ) {
        NSLog("AsterQuickLook: WebView provisional load failed: %@", String(describing: error))
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

    private func preparePreview(at url: URL) throws {
        // Accessing `view` forces NSViewController to load it on macOS 11+.
        _ = view
        removeTemporaryPreview()

        let markdown = try String(contentsOf: url, encoding: .utf8)
        NSLog("AsterQuickLook: read %ld Markdown bytes", markdown.utf8.count)

        let renderedHTML: String = try markdown.withCString { source in
            guard let rendered = aster_markdown_to_html(source) else {
                throw PreviewError.renderFailed
            }
            defer { aster_string_free(rendered) }
            return String(cString: rendered)
        }
        NSLog("AsterQuickLook: rendered %ld HTML bytes", renderedHTML.utf8.count)

        let documentDirectory = URL(
            fileURLWithPath: url.deletingLastPathComponent().path,
            isDirectory: true
        )
        let html = insertingBaseURL(documentDirectory, into: renderedHTML)
        let htmlURL = try writeTemporaryHTML(html)
        previewHTMLURL = htmlURL
        NSLog("AsterQuickLook: temporary HTML %@", htmlURL.path)

        // Loading a real file gives WebKit an explicit file read scope. The
        // generated document uses a <base> tag so relative Markdown assets are
        // still resolved against the source document's directory.
        let readAccessRoot = URL(fileURLWithPath: "/", isDirectory: true)
        guard webView.loadFileURL(htmlURL, allowingReadAccessTo: readAccessRoot) != nil else {
            throw PreviewError.renderFailed
        }

        NSLog("AsterQuickLook: WebView navigation started")
    }

    private func writeTemporaryHTML(_ html: String) throws -> URL {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("AsterQuickLook", isDirectory: true)
        try FileManager.default.createDirectory(
            at: directory,
            withIntermediateDirectories: true
        )

        let fileURL = directory
            .appendingPathComponent(UUID().uuidString)
            .appendingPathExtension("html")
        try html.write(to: fileURL, atomically: true, encoding: .utf8)
        return fileURL
    }

    private func insertingBaseURL(_ baseURL: URL, into html: String) -> String {
        guard let head = html.range(of: "<head>") else { return html }

        let escapedURL = baseURL.absoluteString
            .replacingOccurrences(of: "&", with: "&amp;")
            .replacingOccurrences(of: "\"", with: "&quot;")
            .replacingOccurrences(of: "<", with: "&lt;")
            .replacingOccurrences(of: ">", with: "&gt;")

        var result = html
        result.insert(contentsOf: "<base href=\"\(escapedURL)\">", at: head.upperBound)
        return result
    }

    private func removeTemporaryPreview() {
        guard let previewHTMLURL else { return }
        try? FileManager.default.removeItem(at: previewHTMLURL)
        self.previewHTMLURL = nil
    }
}
