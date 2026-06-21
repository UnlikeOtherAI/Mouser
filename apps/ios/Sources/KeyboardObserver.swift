import Combine
import UIKit

/// Publishes the live height of the system keyboard so the portrait layout can
/// reserve exactly the space *above* it for the touchpad (audit R2 — "portrait
/// keyboard-below layout").
///
/// The brief's portrait "Bed Mode" is: touchpad area ABOVE, the native iOS
/// keyboard BELOW. SwiftUI's `keyboardLayoutGuide` lives on `UIView` and isn't
/// directly reachable from a plain `VStack`, and the default keyboard-avoidance
/// just slides content up (it doesn't *size* the touchpad to the gap). So we
/// observe the keyboard frame notifications and republish the on-screen height;
/// `CompanionView` drives its split off this value, so the touchpad always fills
/// the area the keyboard leaves and the capture field sits just above it.
///
/// Height is reported in the window's coordinate space and clamped to the part of
/// the keyboard that actually overlaps the screen (it is 0 while the keyboard is
/// off-screen, e.g. a hardware keyboard is attached or the field is unfocused).
@MainActor
final class KeyboardObserver: ObservableObject {
    /// On-screen keyboard height in points (0 when no software keyboard is shown).
    @Published private(set) var height: CGFloat = 0
    /// The animation duration the system is using for the current frame change,
    /// so the layout can animate in lock-step with the keyboard.
    @Published private(set) var animationDuration: Double = 0.25

    private var cancellables: Set<AnyCancellable> = []

    init() {
        let center = NotificationCenter.default
        // willChangeFrame covers show, hide, and interactive/height changes
        // (e.g. predictive bar, floating keyboard) in one path.
        center.publisher(for: UIResponder.keyboardWillChangeFrameNotification)
            .merge(with: center.publisher(for: UIResponder.keyboardWillShowNotification))
            .sink { [weak self] note in self?.apply(note) }
            .store(in: &cancellables)

        center.publisher(for: UIResponder.keyboardWillHideNotification)
            .sink { [weak self] note in self?.applyHide(note) }
            .store(in: &cancellables)
    }

    private func apply(_ note: Notification) {
        guard let frame = endFrame(note) else { return }
        animationDuration = duration(note)
        // Intersect the keyboard frame with the active screen so an off-screen
        // (docked-away / hardware) keyboard reports height 0.
        let screen = activeScreenBounds()
        let overlap = screen.intersection(frame)
        height = overlap.isNull ? 0 : overlap.height
    }

    private func applyHide(_ note: Notification) {
        animationDuration = duration(note)
        height = 0
    }

    private func endFrame(_ note: Notification) -> CGRect? {
        (note.userInfo?[UIResponder.keyboardFrameEndUserInfoKey] as? NSValue)?.cgRectValue
    }

    private func duration(_ note: Notification) -> Double {
        (note.userInfo?[UIResponder.keyboardAnimationDurationUserInfoKey] as? Double) ?? 0.25
    }

    private func activeScreenBounds() -> CGRect {
        let scene = UIApplication.shared.connectedScenes
            .compactMap { $0 as? UIWindowScene }
            .first { $0.activationState == .foregroundActive }
        return scene?.screen.bounds ?? UIScreen.main.bounds
    }
}
