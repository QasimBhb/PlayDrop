import AVFoundation
import SwiftUI
import UIKit

// Wraps the QR scanner in the camera permission flow: explain and ask when the user has not
// decided, show the scanner when allowed, and when denied or restricted say so and offer the
// Settings app (anti-pattern 18: never a black box that silently does nothing). The camera is
// asked for only when the user opens the Scan QR tab.

/// Shows `scanner` once camera access is granted, otherwise explains and asks.
struct CameraGate<Scanner: View>: View {
    /// The view to show with camera access.
    @ViewBuilder let scanner: () -> Scanner

    @State private var status = AVCaptureDevice.authorizationStatus(for: .video)
    @Environment(\.openURL) private var openURL
    @Environment(\.scenePhase) private var scenePhase

    var body: some View {
        Group {
            switch status {
            case .authorized:
                scanner()
            case .notDetermined:
                prompt(L10n.Qr.cameraNeeded, button: L10n.Qr.allowCamera) {
                    Task {
                        _ = await AVCaptureDevice.requestAccess(for: .video)
                        status = AVCaptureDevice.authorizationStatus(for: .video)
                    }
                }
            case .restricted:
                prompt(L10n.Qr.cameraRestricted, button: nil) {}
            default:
                prompt(L10n.Qr.cameraDenied, button: L10n.Qr.openSettings) {
                    if let url = URL(string: UIApplication.openSettingsURLString) { openURL(url) }
                }
            }
        }
        .onChange(of: scenePhase) { _, phase in
            if phase == .active { status = AVCaptureDevice.authorizationStatus(for: .video) }
        }
    }

    private func prompt(_ text: String, button: String?, action: @escaping () -> Void) -> some View {
        VStack(spacing: 12) {
            Image(systemName: "camera")
                .font(.largeTitle)
                .foregroundStyle(.secondary)
                .accessibilityHidden(true)
            Text(text)
                .multilineTextAlignment(.center)
            if let button {
                Button(button, action: action)
                    .buttonStyle(.borderedProminent)
            }
        }
        .padding()
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}
