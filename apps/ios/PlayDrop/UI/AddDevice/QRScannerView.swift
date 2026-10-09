@preconcurrency import AVFoundation
import SwiftUI
import UIKit

// The camera view of the Scan QR tab: an `AVCaptureSession` with a metadata output limited
// to QR codes and a full-bleed preview. Every decoded string goes to `onCode`; the caller
// decides whether it is a pairing code (`PairingUri.isPairingUri`) and never logs it (SR-32).
// The session runs only while the view is on screen, on its own serial queue as AVFoundation
// asks (`startRunning` blocks). Camera permission is handled by `CameraGate` around it.

/// A live camera preview that reports decoded QR codes.
struct QRScannerView: UIViewControllerRepresentable {
    /// Called on the main actor with every decoded QR string.
    let onCode: (String) -> Void
    /// Called once when the camera cannot be set up.
    let onFailure: () -> Void

    func makeUIViewController(context: Context) -> QRScannerController {
        let controller = QRScannerController()
        controller.onCode = onCode
        controller.onFailure = onFailure
        return controller
    }

    func updateUIViewController(_ controller: QRScannerController, context: Context) {
        controller.onCode = onCode
        controller.onFailure = onFailure
    }

    static func dismantleUIViewController(_ controller: QRScannerController, coordinator: ()) {
        controller.stopCapture()
    }
}

/// Owns the capture session; see the file comment.
final class QRScannerController: UIViewController, AVCaptureMetadataOutputObjectsDelegate {
    /// Receives decoded strings.
    var onCode: ((String) -> Void)?
    /// Receives a setup failure.
    var onFailure: (() -> Void)?

    private let capture = CaptureSessionBox()
    private var previewLayer: AVCaptureVideoPreviewLayer?
    private var configured = false

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .black
        configured = configure()
        if !configured { onFailure?() }
    }

    override func viewDidLayoutSubviews() {
        super.viewDidLayoutSubviews()
        previewLayer?.frame = view.bounds
    }

    override func viewWillAppear(_ animated: Bool) {
        super.viewWillAppear(animated)
        if configured { capture.start() }
    }

    override func viewWillDisappear(_ animated: Bool) {
        super.viewWillDisappear(animated)
        stopCapture()
    }

    /// Stops the camera; safe to call more than once.
    func stopCapture() {
        capture.stop()
    }

    private func configure() -> Bool {
        guard let device = AVCaptureDevice.default(for: .video),
              let input = try? AVCaptureDeviceInput(device: device)
        else { return false }
        let session = capture.session
        session.beginConfiguration()
        defer { session.commitConfiguration() }
        guard session.canAddInput(input) else { return false }
        session.addInput(input)
        let output = AVCaptureMetadataOutput()
        guard session.canAddOutput(output) else { return false }
        session.addOutput(output)
        // Delivered on the main queue, so the delegate can hop onto the main actor directly.
        output.setMetadataObjectsDelegate(self, queue: .main)
        guard output.availableMetadataObjectTypes.contains(.qr) else { return false }
        output.metadataObjectTypes = [.qr]

        let layer = AVCaptureVideoPreviewLayer(session: session)
        layer.videoGravity = .resizeAspectFill
        layer.frame = view.bounds
        view.layer.addSublayer(layer)
        previewLayer = layer
        return true
    }

    nonisolated func metadataOutput(
        _ output: AVCaptureMetadataOutput,
        didOutput metadataObjects: [AVMetadataObject],
        from connection: AVCaptureConnection
    ) {
        let codes = metadataObjects.compactMap { ($0 as? AVMetadataMachineReadableCodeObject)?.stringValue }
        guard !codes.isEmpty else { return }
        // The delegate queue is `.main` (see `configure`), so this is the main thread.
        MainActor.assumeIsolated {
            for code in codes { self.onCode?(code) }
        }
    }
}

/// The capture session plus the serial queue every start and stop runs on. Unchecked
/// Sendable because the session is touched only on that queue once configured.
private final class CaptureSessionBox: @unchecked Sendable {
    let session = AVCaptureSession()
    private let queue = DispatchQueue(label: "org.playdrop.ios.qr-capture")

    func start() {
        queue.async { [session] in
            if !session.isRunning { session.startRunning() }
        }
    }

    func stop() {
        queue.async { [session] in
            if session.isRunning { session.stopRunning() }
        }
    }
}
