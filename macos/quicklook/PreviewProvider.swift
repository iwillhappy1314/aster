import Foundation
import QuickLookUI
import UniformTypeIdentifiers
import AsterMarkdownBridge

private enum PreviewError: Error {
    case renderFailed
    case encodingFailed
}

@objc(AsterQuickLookPreviewProvider)
final class PreviewProvider: QLPreviewProvider, QLPreviewingController {
    override func providePreview(
        for request: QLFilePreviewRequest,
        completionHandler handler: @escaping (QLPreviewReply?, Error?) -> Void
    ) {
        let sourceURL = request.fileURL
        NSLog("AsterQuickLook: providePreview %@", sourceURL.path)

        let reply = QLPreviewReply(
            dataOfContentType: .html,
            contentSize: CGSize(width: 900, height: 700)
        ) { _ in
            NSLog("AsterQuickLook: rendering %@", sourceURL.path)

            let markdown = try String(contentsOf: sourceURL, encoding: .utf8)
            NSLog("AsterQuickLook: read %ld Markdown bytes", markdown.utf8.count)

            let html: String = try markdown.withCString { source in
                guard let rendered = aster_markdown_to_html(source) else {
                    throw PreviewError.renderFailed
                }
                defer { aster_string_free(rendered) }
                return String(cString: rendered)
            }
            NSLog("AsterQuickLook: rendered %ld HTML bytes", html.utf8.count)

            guard let data = html.data(using: .utf8) else {
                throw PreviewError.encodingFailed
            }
            return data
        }

        reply.title = sourceURL.lastPathComponent
        NSLog("AsterQuickLook: returning HTML reply")
        handler(reply, nil)
    }
}
