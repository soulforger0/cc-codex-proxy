import AppKit
import Combine
import SwiftUI

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    private let model = ProxyAppModel()
    private var statusItemController: StatusItemController?

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.accessory)
        statusItemController = StatusItemController(model: model)
        Task {
            await model.refresh()
            await model.installClaudeShim()
        }
    }

    func applicationWillTerminate(_ notification: Notification) {
        model.restoreClaudeShimForTermination()
    }
}

@MainActor
final class StatusItemController: NSObject {
    private let model: ProxyAppModel
    private let statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
    private let popover = NSPopover()
    private var cancellables = Set<AnyCancellable>()

    init(model: ProxyAppModel) {
        self.model = model
        super.init()
        configureStatusItem()
        configurePopover()
        bindModel()
    }

    private func configureStatusItem() {
        guard let button = statusItem.button else { return }
        button.target = self
        button.action = #selector(statusItemClicked(_:))
        button.sendAction(on: [.leftMouseUp, .rightMouseUp])
        button.imagePosition = .imageOnly
        button.toolTip = "CC Codex Proxy"
        updateStatusIcon()
    }

    private func configurePopover() {
        popover.behavior = .transient
        popover.animates = true
        popover.contentSize = NSSize(width: 440, height: 660)
        popover.contentViewController = NSHostingController(
            rootView: ContentView()
                .environmentObject(model)
                .frame(width: 440, height: 660)
        )
    }

    private func bindModel() {
        model.$isRunning
            .receive(on: RunLoop.main)
            .sink { [weak self] _ in
                self?.updateStatusIcon()
            }
            .store(in: &cancellables)

        model.$statusText
            .receive(on: RunLoop.main)
            .sink { [weak self] _ in
                self?.updateStatusTooltip()
            }
            .store(in: &cancellables)
    }

    private func updateStatusIcon() {
        let imageName = model.isRunning ? "bolt.horizontal.circle.fill" : "bolt.horizontal.circle"
        let image = NSImage(systemSymbolName: imageName, accessibilityDescription: statusAccessibilityDescription)
        image?.isTemplate = true
        statusItem.button?.image = image
        updateStatusTooltip()
    }

    private func updateStatusTooltip() {
        statusItem.button?.toolTip = "CC Codex Proxy - \(model.statusText)"
    }

    private var statusAccessibilityDescription: String {
        model.isRunning ? "CC Codex Proxy running" : "CC Codex Proxy stopped"
    }

    @objc private func statusItemClicked(_ sender: NSStatusBarButton) {
        if NSApp.currentEvent?.type == .rightMouseUp {
            showContextMenu()
        } else {
            togglePopover(relativeTo: sender)
        }
    }

    private func togglePopover(relativeTo button: NSStatusBarButton) {
        if popover.isShown {
            popover.performClose(nil)
            return
        }

        popover.show(relativeTo: button.bounds, of: button, preferredEdge: .minY)
        Task { await model.refresh() }
    }

    private func showContextMenu() {
        if popover.isShown {
            popover.performClose(nil)
        }

        let menu = NSMenu()
        menu.autoenablesItems = false

        let status = NSMenuItem(title: contextStatusTitle, action: nil, keyEquivalent: "")
        status.isEnabled = false
        menu.addItem(status)
        menu.addItem(.separator())

        menu.addItem(contextMenuItem(
            title: "Start",
            systemImage: "play.fill",
            action: #selector(startProxyFromMenu),
            isEnabled: !model.isRunning
        ))
        menu.addItem(contextMenuItem(
            title: "Stop",
            systemImage: "stop.fill",
            action: #selector(stopProxyFromMenu),
            isEnabled: model.isRunning
        ))
        menu.addItem(contextMenuItem(
            title: "Status",
            systemImage: "arrow.clockwise",
            action: #selector(refreshStatusFromMenu),
            isEnabled: true
        ))

        menu.addItem(.separator())
        menu.addItem(contextMenuItem(
            title: "Quit",
            systemImage: "power",
            action: #selector(quitFromMenu),
            isEnabled: true
        ))

        statusItem.menu = menu
        statusItem.button?.performClick(nil)
        statusItem.menu = nil
    }

    private var contextStatusTitle: String {
        "Status: \(model.statusText)"
    }

    private func contextMenuItem(
        title: String,
        systemImage: String,
        action: Selector,
        isEnabled: Bool
    ) -> NSMenuItem {
        let item = NSMenuItem(title: title, action: action, keyEquivalent: "")
        item.target = self
        item.isEnabled = isEnabled
        item.image = NSImage(systemSymbolName: systemImage, accessibilityDescription: title)
        return item
    }

    @objc private func startProxyFromMenu() {
        Task { await model.startProxy() }
    }

    @objc private func stopProxyFromMenu() {
        Task { await model.stopProxy() }
    }

    @objc private func refreshStatusFromMenu() {
        Task { await model.refresh() }
    }

    @objc private func quitFromMenu() {
        NSApplication.shared.terminate(nil)
    }
}
