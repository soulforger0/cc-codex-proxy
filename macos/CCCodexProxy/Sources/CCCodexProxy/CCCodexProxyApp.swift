import SwiftUI

@main
struct CCCodexProxyApp: App {
    @StateObject private var model = ProxyAppModel()

    var body: some Scene {
        MenuBarExtra {
            ContentView()
                .environmentObject(model)
                .frame(width: 420, height: 640)
                .task {
                    await model.refresh()
                }
        } label: {
            Image(systemName: model.isRunning ? "bolt.horizontal.circle.fill" : "bolt.horizontal.circle")
        }
        .menuBarExtraStyle(.window)
    }
}
