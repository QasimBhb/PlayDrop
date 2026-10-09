import PlayDropShared
import SwiftUI
import UIKit

// Principal class of the share extension (NSExtensionPrincipalClass is
// `$(PRODUCT_MODULE_NAME).ShareViewController`, so the class keeps its Swift name). It hosts
// `ShareView` and connects its buttons to the extension context. The extension never starts
// the core: `ShareModel` copies the items into the App Group queue and the app sends them
// when it is opened (SR-30, PLAN.md iOS share extension row).

/// Hosts the share card; see the file comment.
final class ShareViewController: UIViewController {
    private let model = ShareModel()
    private var finished = false

    override func viewDidLoad() {
        super.viewDidLoad()
        let root = ShareView(
            model: model,
            onOpenApp: { [weak self] in self?.openApp() },
            onDone: { [weak self] in self?.done() },
            onCancel: { [weak self] in self?.cancel() }
        )
        let host = UIHostingController(rootView: root)
        addChild(host)
        host.view.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(host.view)
        NSLayoutConstraint.activate([
            host.view.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            host.view.trailingAnchor.constraint(equalTo: view.trailingAnchor),
            host.view.topAnchor.constraint(equalTo: view.topAnchor),
            host.view.bottomAnchor.constraint(equalTo: view.bottomAnchor),
        ])
        host.didMove(toParent: self)

        let items = (extensionContext?.inputItems ?? []).compactMap { $0 as? NSExtensionItem }
        model.start(items: items)
    }

    /// Opens PlayDrop (best effort, see `AppOpener`), then closes the sheet.
    private func openApp() {
        guard let url = URL(string: "\(PlayDropIdentifiers.urlScheme)://") else {
            done()
            return
        }
        AppOpener.open(url, from: self, context: extensionContext) { [weak self] _ in
            // Whether or not the app came forward, the batch is committed; close the sheet.
            self?.done()
        }
    }

    /// Closes the sheet and keeps the committed batch for the app.
    private func done() {
        guard !finished else { return }
        finished = true
        extensionContext?.completeRequest(returningItems: [], completionHandler: nil)
    }

    /// Aborts the batch and tells the host app the user cancelled.
    private func cancel() {
        guard !finished else { return }
        finished = true
        model.cancel()
        let error = NSError(domain: NSCocoaErrorDomain, code: NSUserCancelledError)
        extensionContext?.cancelRequest(withError: error)
    }
}
