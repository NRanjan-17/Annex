import AppKit
import SwiftUI

/// A native, left-aligned text field. SwiftUI's grouped Form right-aligns its own fields.
struct InputField: NSViewRepresentable {
    @Binding var text: String
    var placeholder: String
    /// Return pressed or focus left the field.
    var onCommit: () -> Void = {}
    /// Return pressed.
    var onSubmit: () -> Void = {}

    func makeNSView(context: Context) -> NSTextField {
        let f = NSTextField()
        f.bezelStyle = .roundedBezel
        f.isBezeled = true
        f.alignment = .left
        f.font = .systemFont(ofSize: NSFont.systemFontSize)
        f.lineBreakMode = .byTruncatingTail
        f.usesSingleLineMode = true
        f.cell?.isScrollable = true
        f.delegate = context.coordinator
        return f
    }

    func updateNSView(_ f: NSTextField, context: Context) {
        context.coordinator.parent = self
        if f.stringValue != text { f.stringValue = text }
        f.placeholderString = placeholder
    }

    func makeCoordinator() -> Coordinator { Coordinator(self) }

    final class Coordinator: NSObject, NSTextFieldDelegate {
        var parent: InputField
        init(_ parent: InputField) { self.parent = parent }

        func controlTextDidChange(_ note: Notification) {
            if let f = note.object as? NSTextField { parent.text = f.stringValue }
        }

        func controlTextDidEndEditing(_ note: Notification) {
            parent.onCommit()
        }

        func control(_ control: NSControl, textView: NSTextView, doCommandBy selector: Selector) -> Bool {
            if selector == #selector(NSResponder.insertNewline(_:)) {
                parent.onSubmit()
            }
            return false
        }
    }
}
