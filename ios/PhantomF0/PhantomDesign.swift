import SwiftUI

/// Silhouette vettoriale ufficiale di Phantom (Opzione A2 · Ghost Pure: cupola geometrica, 3 onde simmetriche, occhi minimali).
struct PhantomGhostMark: Shape {
    func path(in rect: CGRect) -> Path {
        let s = min(rect.width, rect.height)
        let ox = rect.midX - s / 2
        let oy = rect.midY - s / 2
        func p(_ x: CGFloat, _ y: CGFloat) -> CGPoint {
            CGPoint(x: ox + (x / 108.0) * s, y: oy + (y / 108.0) * s)
        }

        var path = Path()
        // Corpo del fantasmino (coordinate normalizzate 0...108 identiche al vettoriale Android)
        path.move(to: p(54, 16))
        path.addCurve(to: p(25, 45), control1: p(37.5, 16), control2: p(25, 28.2))
        path.addLine(to: p(25, 82))
        path.addCurve(to: p(36.8, 86.8), control1: p(25, 88.2), control2: p(32.6, 91.0))
        path.addCurve(to: p(47.2, 86.8), control1: p(39.8, 83.8), control2: p(44.2, 83.8))
        path.addCurve(to: p(60.8, 86.8), control1: p(51.0, 90.6), control2: p(57.0, 90.6))
        path.addCurve(to: p(71.2, 86.8), control1: p(63.8, 83.8), control2: p(68.2, 83.8))
        path.addCurve(to: p(83, 82), control1: p(75.4, 91.0), control2: p(83, 88.2))
        path.addLine(to: p(83, 45))
        path.addCurve(to: p(54, 16), control1: p(83, 28.2), control2: p(70.5, 16))
        path.closeSubpath()

        // Occhi minimali a pillola (ritagliati tramite FillStyle(eoFill: true))
        let eyeW = (8.4 / 108.0) * s
        let eyeH = (16.0 / 108.0) * s
        let eyeR = eyeW / 2
        let leftEye = CGRect(x: ox + (39.8 / 108.0) * s, y: oy + (39.0 / 108.0) * s, width: eyeW, height: eyeH)
        let rightEye = CGRect(x: ox + (59.8 / 108.0) * s, y: oy + (39.0 / 108.0) * s, width: eyeW, height: eyeH)
        path.addRoundedRect(in: leftEye, cornerSize: CGSize(width: eyeR, height: eyeR))
        path.addRoundedRect(in: rightEye, cornerSize: CGSize(width: eyeR, height: eyeR))
        return path
    }
}

/// Palette monocromatica adattiva (Light & Dark Mode) ispirata a Leica / Nothing / Apple Pro.
enum PhantomPalette {
    static func canvas(_ scheme: ColorScheme) -> Color {
        scheme == .dark ? Color.black : Color(red: 0.961, green: 0.961, blue: 0.969)
    }

    static func cardFill(_ scheme: ColorScheme) -> Color {
        scheme == .dark ? Color.white.opacity(0.065) : Color.white.opacity(0.86)
    }

    static func elevatedFill(_ scheme: ColorScheme) -> Color {
        scheme == .dark ? Color.white.opacity(0.10) : Color.black.opacity(0.055)
    }

    static func ctaBackground(_ scheme: ColorScheme) -> Color {
        scheme == .dark ? Color.white : Color(red: 0.04, green: 0.04, blue: 0.05)
    }

    static func ctaForeground(_ scheme: ColorScheme) -> Color {
        scheme == .dark ? Color(red: 0.04, green: 0.04, blue: 0.05) : Color.white
    }

    static func specularRim(_ scheme: ColorScheme) -> LinearGradient {
        if scheme == .dark {
            return LinearGradient(
                colors: [Color.white.opacity(0.28), Color.white.opacity(0.06), Color.white.opacity(0.12)],
                startPoint: .topLeading,
                endPoint: .bottomTrailing
            )
        }
        return LinearGradient(
            colors: [Color.white.opacity(0.95), Color.black.opacity(0.07), Color.black.opacity(0.04)],
            startPoint: .topLeading,
            endPoint: .bottomTrailing
        )
    }

    static let liveGreen = Color(red: 0.188, green: 0.820, blue: 0.345)
    static let warnAmber = Color(red: 1.0, green: 0.624, blue: 0.039)
    static let dangerRed = Color(red: 1.0, green: 0.271, blue: 0.227)
}

/// Superficie Liquid Glass coerente su iOS 17–26: materiale traslucido, riflesso speculare hairline e ombra leggera.
struct PhantomGlassSurface: ViewModifier {
    @Environment(\.colorScheme) private var scheme
    var cornerRadius: CGFloat = 22
    var overVideo: Bool = false

    func body(content: Content) -> some View {
        let shape = RoundedRectangle(cornerRadius: cornerRadius, style: .continuous)
        content
            .background {
                ZStack {
                    if overVideo {
                        shape.fill(.ultraThinMaterial)
                        shape.fill(Color.black.opacity(0.28))
                    } else {
                        shape.fill(.regularMaterial)
                        shape.fill(PhantomPalette.cardFill(scheme))
                    }
                }
            }
            .overlay {
                shape.strokeBorder(
                    overVideo
                        ? LinearGradient(
                            colors: [Color.white.opacity(0.36), Color.white.opacity(0.08), Color.white.opacity(0.16)],
                            startPoint: .topLeading,
                            endPoint: .bottomTrailing
                        )
                        : PhantomPalette.specularRim(scheme),
                    lineWidth: 0.65
                )
            }
            .shadow(
                color: Color.black.opacity(overVideo ? 0.32 : (scheme == .dark ? 0.35 : 0.06)),
                radius: overVideo ? 18 : 14,
                x: 0,
                y: overVideo ? 8 : 5
            )
    }
}

/// Pillola Liquid Glass traslucida per indicatori di stato e HUD collassato.
struct PhantomGlassCapsule: ViewModifier {
    @Environment(\.colorScheme) private var scheme
    var overVideo: Bool = false

    func body(content: Content) -> some View {
        content
            .background {
                ZStack {
                    if overVideo {
                        Capsule().fill(.ultraThinMaterial)
                        Capsule().fill(Color.black.opacity(0.30))
                    } else {
                        Capsule().fill(.thinMaterial)
                        Capsule().fill(PhantomPalette.elevatedFill(scheme))
                    }
                }
            }
            .overlay {
                Capsule().strokeBorder(
                    overVideo
                        ? LinearGradient(
                            colors: [Color.white.opacity(0.38), Color.white.opacity(0.10)],
                            startPoint: .top,
                            endPoint: .bottom
                        )
                        : PhantomPalette.specularRim(scheme),
                    lineWidth: 0.65
                )
            }
            .shadow(
                color: Color.black.opacity(overVideo ? 0.30 : (scheme == .dark ? 0.25 : 0.05)),
                radius: 12,
                x: 0,
                y: 4
            )
    }
}

/// Pulsante primario monocromatico ad alto contrasto (bianco in Dark, nero in Light) con risposta elastica.
struct PhantomPrimaryButtonStyle: ButtonStyle {
    @Environment(\.colorScheme) private var scheme

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.subheadline.weight(.semibold))
            .foregroundStyle(PhantomPalette.ctaForeground(scheme))
            .padding(.horizontal, 20)
            .padding(.vertical, 13)
            .background(PhantomPalette.ctaBackground(scheme), in: Capsule())
            .scaleEffect(configuration.isPressed ? 0.97 : 1.0)
            .opacity(configuration.isPressed ? 0.88 : 1.0)
            .animation(.spring(response: 0.22, dampingFraction: 0.82), value: configuration.isPressed)
    }
}

/// Pulsante secondario Liquid Glass con feedback tattile leggero.
struct PhantomSecondaryButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.subheadline.weight(.medium))
            .foregroundStyle(.primary)
            .padding(.horizontal, 16)
            .padding(.vertical, 11)
            .modifier(PhantomGlassCapsule())
            .scaleEffect(configuration.isPressed ? 0.97 : 1.0)
            .animation(.spring(response: 0.22, dampingFraction: 0.82), value: configuration.isPressed)
    }
}

extension View {
    func phantomGlassCard(cornerRadius: CGFloat = 22, overVideo: Bool = false) -> some View {
        modifier(PhantomGlassSurface(cornerRadius: cornerRadius, overVideo: overVideo))
    }

    func phantomGlassCapsule(overVideo: Bool = false) -> some View {
        modifier(PhantomGlassCapsule(overVideo: overVideo))
    }
}
