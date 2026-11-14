// UDM's side of Safari's native messaging, in the app extension that
// installer/macos/safari/build.sh puts in UDM.app/Contents/PlugIns. It takes
// the place of the handler Xcode's safari-web-extension-converter writes.
//
// Safari hands each runtime.sendNativeMessage of the extension's worker to
// beginRequest, and its answer back; nothing here can send the worker a
// message of its own. So the worker keeps a poll out
// (extension/chromium/native.js), and this keeps one `udm-host safari`
// running between requests:
//
//   { "frames": [message, ...] }  written to udm-host's standard input;
//                                  answered at once, without frames
//   { "poll": true }               answered with every frame udm-host wrote
//                                  to its standard output since the last
//                                  poll, as soon as there is one, or after
//                                  `pollWait` with none
//   "start": true                  on a port's first request: a new
//                                  udm-host, so nothing an earlier worker
//                                  left unread reaches this one
//   answer: { "session": id, "frames": [...], "more": bool, "error": text }
//
// `session` names the udm-host that answered, so the worker knows to say
// hello again to a new one; `error` says that udm-host stopped, and why (the
// last line it wrote to standard error), and the next request starts another.
// A udm-host nobody asks for `idleLimit`, or whose frames pile up unread
// past `Connection.bufferLimit`, is stopped.
//
// The app extension runs in Safari's sandbox, and so does udm-host, which
// it starts (udm-host.entitlements). udm-host starts no daemon there: UDM
// must already be running.

import Foundation
import SafariServices

/// The key of a native message in an extension item's userInfo: the value
/// of SFExtensionMessageKey, spelled out so that older SDKs build this too.
private let messageKey = "message"

final class SafariWebExtensionHandler: NSObject, NSExtensionRequestHandling {
    func beginRequest(with context: NSExtensionContext) {
        let item = context.inputItems.first as? NSExtensionItem
        let request = item?.userInfo?[messageKey] as? [String: Any] ?? [:]
        Host.shared.exchange(request) { reply in
            let response = NSExtensionItem()
            response.userInfo = [messageKey: reply]
            context.completeRequest(returningItems: [response], completionHandler: nil)
        }
    }
}

/// The one udm-host of this app extension, and the requests that use it. A
/// poll waits while messages go out beside it.
private final class Host {
    static let shared = Host()

    /// How long a poll waits for udm-host's next frame. The worker gives up
    /// on a request after 30 seconds (SAFARI_REQUEST_TIMEOUT_MS).
    private let pollWait: TimeInterval = 20
    /// How long udm-host runs with no request in progress or made: longer
    /// than a worker takes between two polls.
    private let idleLimit: TimeInterval = 60

    private let lock = NSLock()
    // Guarded by `lock`.
    private var connection: Connection?
    private var active = 0
    private var lastSeen = Date()

    private init() {
        // A write to a udm-host that just stopped must fail, not end this
        // process.
        signal(SIGPIPE, SIG_IGN)
        Thread { self.stopWhenIdle() }.start()
    }

    func exchange(_ request: [String: Any], reply: @escaping ([String: Any]) -> Void) {
        DispatchQueue.global(qos: .userInitiated).async { reply(self.answer(request)) }
    }

    private func answer(_ request: [String: Any]) -> [String: Any] {
        let frames = request["frames"] as? [Any] ?? []
        let poll = request["poll"] as? Bool ?? false
        let start = request["start"] as? Bool ?? false

        lock.lock()
        active += 1
        lastSeen = Date()
        let replaced = start ? connection : nil
        if start {
            connection = nil
        }
        var current = connection
        var failure: String?
        if current == nil {
            do {
                let started = try Connection(program: Host.program())
                connection = started
                current = started
            } catch {
                failure = "Could not start udm-host: \(error.localizedDescription)"
            }
        }
        lock.unlock()
        replaced?.stop(because: "another worker started")
        defer {
            lock.lock()
            active -= 1
            lastSeen = Date()
            lock.unlock()
        }
        guard let host = current else {
            return ["frames": [Any](), "error": failure ?? "Could not start udm-host"]
        }

        var reply: [String: Any] = ["session": host.session, "frames": [Any]()]
        var ended: String?
        if !host.send(frames) {
            // A write that failed means udm-host stopped: wait for its reason.
            ended = host.ended(waiting: 1) ?? "udm-host stopped"
        } else if poll {
            let taken = host.take(waiting: pollWait)
            reply["frames"] = taken.frames
            reply["more"] = taken.more
            ended = taken.ended
        } else {
            ended = host.ended(waiting: 0)
        }
        if let ended = ended {
            reply["error"] = ended
            forget(host)
        }
        return reply
    }

    /// Start a new udm-host for the next request, unless one already has.
    private func forget(_ host: Connection) {
        lock.lock()
        if connection === host {
            connection = nil
        }
        lock.unlock()
    }

    private func stopWhenIdle() {
        while true {
            Thread.sleep(forTimeInterval: 10)
            lock.lock()
            var idle: Connection?
            if active == 0, Date().timeIntervalSince(lastSeen) > idleLimit {
                idle = connection
                connection = nil
            }
            lock.unlock()
            idle?.stop(because: "nobody asked for UDM's news")
        }
    }

    /// udm-host: the copy signed for the sandbox inside this app extension,
    /// else UDM.app's own (this app extension is in UDM.app/Contents/PlugIns).
    private static func program() -> URL {
        if let own = Bundle.main.url(forAuxiliaryExecutable: "udm-host") {
            return own
        }
        return Bundle.main.bundleURL
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .appendingPathComponent("MacOS/udm-host")
    }
}

/// One running udm-host: what goes to it, and the frames and the end it
/// reported, which its reader threads collect.
private final class Connection {
    /// The most a reply carries, so a long list does not make one message
    /// too large: the rest waits for the next poll (`more`).
    private static let replyBytes = 4 << 20
    /// The most frames wait unread before udm-host is stopped.
    private static let bufferLimit = 16 << 20
    /// The largest frame udm-host writes; a larger length means the stream
    /// is broken.
    private static let frameBytes = 64 << 20

    let session = UUID().uuidString
    private let process = Process()
    private let input = Pipe()
    private let output = Pipe()
    private let errors = Pipe()
    private let errorsRead = DispatchGroup()
    private let writing = NSLock()
    private let state = NSCondition()
    // Guarded by `state`.
    private var frames: [(object: Any, bytes: Int)] = []
    private var buffered = 0
    private var ended: String?
    private var stopReason: String?
    private var errorText = ""

    init(program: URL) throws {
        process.executableURL = program
        process.arguments = ["safari"]
        // In the sandbox HOME is this app extension's container; UDM's data
        // folder, with the daemon's token, is in the user's own.
        var environment = ProcessInfo.processInfo.environment
        if let home = Connection.userHome() {
            environment["HOME"] = home
        }
        process.environment = environment
        process.standardInput = input
        process.standardOutput = output
        process.standardError = errors
        try process.run()
        errorsRead.enter()
        Thread { self.readErrors() }.start()
        Thread { self.readFrames() }.start()
    }

    /// Stop udm-host; its end says `reason`.
    func stop(because reason: String) {
        state.lock()
        if stopReason == nil {
            stopReason = reason
        }
        state.unlock()
        process.terminate()
    }

    /// Write `messages` to udm-host as native-messaging frames. False when
    /// it no longer reads them.
    func send(_ messages: [Any]) -> Bool {
        var data = Data()
        for message in messages {
            guard JSONSerialization.isValidJSONObject(message),
                  let json = try? JSONSerialization.data(withJSONObject: message)
            else { continue }
            withUnsafeBytes(of: UInt32(json.count).littleEndian) { data.append(contentsOf: $0) }
            data.append(json)
        }
        if data.isEmpty {
            return true
        }
        writing.lock()
        defer { writing.unlock() }
        do {
            try input.fileHandleForWriting.write(contentsOf: data)
            return true
        } catch {
            return false
        }
    }

    /// The frames udm-host wrote, waiting up to `seconds` for the first;
    /// whether more are left for the next poll; and, once every frame is
    /// taken, why it stopped, if it did.
    func take(waiting seconds: TimeInterval) -> (frames: [Any], more: Bool, ended: String?) {
        state.lock()
        defer { state.unlock() }
        let deadline = Date(timeIntervalSinceNow: seconds)
        while frames.isEmpty, ended == nil, state.wait(until: deadline) {}
        var taken: [Any] = []
        var bytes = 0
        while let first = frames.first, taken.isEmpty || bytes + first.bytes <= Connection.replyBytes {
            taken.append(first.object)
            bytes += first.bytes
            frames.removeFirst()
        }
        buffered -= bytes
        return (frames: taken, more: !frames.isEmpty, ended: frames.isEmpty ? ended : nil)
    }

    /// Why udm-host stopped, waiting up to `seconds` for it to; nil while it
    /// runs.
    func ended(waiting seconds: TimeInterval) -> String? {
        state.lock()
        defer { state.unlock() }
        let deadline = Date(timeIntervalSinceNow: seconds)
        while ended == nil, state.wait(until: deadline) {}
        return ended
    }

    private func readFrames() {
        let handle = output.fileHandleForReading
        var buffer = Data()
        reading: while true {
            let chunk = handle.availableData
            if chunk.isEmpty {
                break
            }
            buffer.append(chunk)
            while buffer.count >= 4 {
                let start = buffer.startIndex
                let length = buffer[start ..< start + 4].reversed().reduce(0) { $0 << 8 | Int($1) }
                if length > Connection.frameBytes {
                    stop(because: "udm-host wrote a broken frame")
                    break reading
                }
                guard buffer.count >= 4 + length else { break }
                let payload = buffer.subdata(in: start + 4 ..< start + 4 + length)
                buffer.removeSubrange(start ..< start + 4 + length)
                guard let object = try? JSONSerialization.jsonObject(with: payload) else { continue }
                state.lock()
                frames.append((object: object, bytes: length))
                buffered += length
                let full = buffered > Connection.bufferLimit
                state.broadcast()
                state.unlock()
                if full {
                    stop(because: "UDM's messages piled up unread")
                    break reading
                }
            }
        }
        process.waitUntilExit()
        _ = errorsRead.wait(timeout: .now() + 1)
        state.lock()
        let last = errorText.split(whereSeparator: \.isNewline).last.map(String.init)
        ended = stopReason ?? last ?? "udm-host stopped (exit status \(process.terminationStatus))"
        state.broadcast()
        state.unlock()
    }

    /// Keep the end of what udm-host writes to standard error: its last line
    /// says why it stopped.
    private func readErrors() {
        let handle = errors.fileHandleForReading
        while true {
            let chunk = handle.availableData
            if chunk.isEmpty {
                break
            }
            state.lock()
            errorText = String((errorText + String(decoding: chunk, as: UTF8.self)).suffix(4096))
            state.unlock()
        }
        errorsRead.leave()
    }

    /// The user's home folder, which HOME does not name in the sandbox.
    private static func userHome() -> String? {
        guard let entry = getpwuid(getuid()), let dir = entry.pointee.pw_dir else { return nil }
        return String(cString: dir)
    }
}
