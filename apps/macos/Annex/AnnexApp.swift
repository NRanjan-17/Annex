import SwiftUI

@main
struct AnnexApp: App {
    @StateObject private var model = AppModel()

    var body: some Scene {
        MenuBarExtra("Annex", systemImage: model.isHosting ? "display.2" : "display") {
            MenuContent(model: model)
                .onAppear { model.refreshDisplays() }
        }
        .menuBarExtraStyle(.window)
    }
}
