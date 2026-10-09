import CoreImage
import CoreImage.CIFilterBuiltins
import SwiftUI
import UIKit

// Draws the one-time pairing QR code (SR-9): the image comes from CoreImage's
// `CIQRCodeGenerator`, sits on a white square inside `SecureContainer` so screenshots and
// recordings leave it out, and a countdown shows its two-minute life. When the code expires
// the view offers a new one. The URI is never logged or copied anywhere (SR-32).

/// Turns a string into a crisp QR code image.
enum QRCodeImage {
    /// The QR image for `text`, each module `scale` pixels wide, or nil when CoreImage fails.
    static func make(_ text: String, scale: CGFloat = 12) -> UIImage? {
        let filter = CIFilter.qrCodeGenerator()
        filter.message = Data(text.utf8)
        filter.correctionLevel = "M"
        guard let output = filter.outputImage else { return nil }
        let scaled = output.transformed(by: CGAffineTransform(scaleX: scale, y: scale))
        let context = CIContext(options: [.useSoftwareRenderer: false])
        guard let cgImage = context.createCGImage(scaled, from: scaled.extent) else { return nil }
        return UIImage(cgImage: cgImage)
    }
}

/// The Show QR tab's content: the code, its countdown and the warning, or "expired".
struct QRCodeView: View {
    /// The live URI, or nil once expired.
    let uri: String?
    /// Seconds before the code stops working.
    let secondsLeft: Int
    /// Asks for a new code.
    let onNewCode: () -> Void

    @State private var image: UIImage?

    var body: some View {
        VStack(spacing: 16) {
            Text(L10n.Qr.showIntro)
                .font(.body)
                .multilineTextAlignment(.center)
            if uri != nil {
                SecureContainer {
                    ZStack {
                        Color.white
                        if let image {
                            Image(uiImage: image)
                                .interpolation(.none)
                                .resizable()
                                .scaledToFit()
                                .padding(16)
                        } else {
                            ProgressView()
                        }
                    }
                }
                .aspectRatio(1, contentMode: .fit)
                .frame(maxWidth: 320)
                .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
                .accessibilityElement(children: .ignore)
                .accessibilityLabel(Text(L10n.Qr.contentDescription))
                .accessibilityAddTraits(.isImage)
                Text(L10n.Qr.validFor(UIFormat.countdown(secondsLeft)))
                    .font(.headline)
                    .foregroundStyle(.tint)
                    .monospacedDigit()
                Text(L10n.Qr.warning)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
            } else {
                Text(L10n.Qr.expired)
                Button(L10n.Qr.newCode, action: onNewCode)
                    .buttonStyle(.borderedProminent)
            }
        }
        .frame(maxWidth: .infinity)
        .task(id: uri) {
            image = uri.flatMap { QRCodeImage.make($0) }
        }
    }
}
