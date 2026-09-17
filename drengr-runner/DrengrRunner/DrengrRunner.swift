import Foundation
import Network
import UIKit
import XCTest

final class DrengrRunner: XCTestCase {
    // A failed XCUI interaction (e.g. typing with no focused field) records an
    // XCTest failure that would otherwise end the test method and kill the
    // long-running server. Continue after failures so one bad action just
    // returns an error to the client and the server keeps serving.
    override func setUp() {
        super.setUp()
        continueAfterFailure = true
    }

    func testServe() throws {
        let port = portFromEnv()
        let server = try HttpServer(port: port)
        register(server)
        try server.start()
        NSLog("[drengr-runner] listening on 127.0.0.1:\(port)")
        while true {
            RunLoop.main.run(mode: .default, before: Date(timeIntervalSinceNow: 0.05))
        }
    }
}

private let MAX_REQUEST_BYTES = 4 * 1_048_576
private let DRIVER_VERSION = "0.6.0"

private func portFromEnv() -> UInt16 {
    if let s = ProcessInfo.processInfo.environment["DRENGR_RUNNER_PORT"],
       let n = UInt16(s) {
        return n
    }
    return 8200
}

// MARK: - Routes

private func register(_ server: HttpServer) {
    server.get("/status") { _ in
        let shot = XCUIScreen.main.screenshot()
        return .json([
            "ok": true,
            "product": "drengr-runner",
            "version": DRIVER_VERSION,
            "ios_major": ProcessInfo.processInfo.operatingSystemVersion.majorVersion,
            "screen": [
                "width": Int(shot.image.size.width),
                "height": Int(shot.image.size.height),
                "scale": shot.image.scale,
            ],
        ])
    }

    server.get("/observe") { req in
        // screenshot_b64 is always present; future fields are additive.
        let shot = XCUIScreen.main.screenshot()
        let img = encodeScreenshot(shot.image)
        // ?bundle=<id> lets the driver name the foreground app so its real
        // elements are visible; without it we fall back to SpringBoard only.
        let tree = treeHint(bundle: req.query["bundle"])
        var body: [String: Any] = ["screenshot_b64": img.base64EncodedString()]
        body["tree_hint"] = tree ?? NSNull()
        return .json(body)
    }

    server.post("/act") { req in
        let json = (try? JSONSerialization.jsonObject(with: req.body) as? [String: Any]) ?? [:]
        let kind = (json["kind"] as? String) ?? ""
        var caught: Error? = nil
        DispatchQueue.main.sync {
            do {
                switch kind {
                case "tap": try act_tap(json)
                case "swipe": try act_swipe(json)
                case "draw_path": try act_draw_path(json)
                case "type": try act_type(json)
                case "button": try act_button(json)
                case "orientation": try act_orientation(json)
                default: throw ActError.bad("unknown action kind: \(kind)")
                }
            } catch { caught = error }
        }
        if let e = caught as? ActError { return .clientError(e.message) }
        if let e = caught { return .clientError("\(e)") }
        return .json(["ok": true])
    }
}

// MARK: - Actions

private enum ActError: Error {
    case missing(String)
    case bad(String)
    var message: String {
        switch self {
        case .missing(let k): return "missing field: \(k)"
        case .bad(let s): return s
        }
    }
}

private func num(_ j: [String: Any], _ key: String) throws -> Double {
    if let v = j[key] as? Double { return v }
    if let v = j[key] as? Int { return Double(v) }
    if let v = j[key] as? NSNumber { return v.doubleValue }
    throw ActError.missing(key)
}

private func springboard() -> XCUIApplication {
    XCUIApplication(bundleIdentifier: "com.apple.springboard")
}

private func coord(_ x: Double, _ y: Double) -> XCUICoordinate {
    let origin = springboard().coordinate(withNormalizedOffset: CGVector(dx: 0, dy: 0))
    return origin.withOffset(CGVector(dx: x, dy: y))
}

private func clampedVelocity(_ duration: Double) -> XCUIGestureVelocity {
    let v = 1.0 / max(duration, 0.01) * 500
    return XCUIGestureVelocity(min(max(v, 50), 4000))
}

private func act_tap(_ j: [String: Any]) throws {
    let x = try num(j, "x"), y = try num(j, "y")
    coord(x, y).tap()
}

private func act_swipe(_ j: [String: Any]) throws {
    let x1 = try num(j, "x1"), y1 = try num(j, "y1")
    let x2 = try num(j, "x2"), y2 = try num(j, "y2")
    let duration = ((try? num(j, "duration_ms")) ?? 300) / 1000.0
    coord(x1, y1).press(
        forDuration: 0.05,
        thenDragTo: coord(x2, y2),
        withVelocity: clampedVelocity(duration),
        thenHoldForDuration: 0
    )
}

// Public-API limitation: each segment lifts the finger between points.
// True continuous strokes require the private XCSynthesizedEventRecord
// path, slated for v0.6.1.
private func act_draw_path(_ j: [String: Any]) throws {
    guard let pts = j["points"] as? [[String: Any]], pts.count >= 2 else {
        throw ActError.bad("points must be a list of at least 2 {x,y}")
    }
    let duration = ((try? num(j, "duration_ms")) ?? 800) / 1000.0
    let perSeg = duration / Double(pts.count - 1)
    var prev = try CGPoint(x: num(pts[0], "x"), y: num(pts[0], "y"))
    for i in 1..<pts.count {
        let next = try CGPoint(x: num(pts[i], "x"), y: num(pts[i], "y"))
        coord(Double(prev.x), Double(prev.y)).press(
            forDuration: 0,
            thenDragTo: coord(Double(next.x), Double(next.y)),
            withVelocity: clampedVelocity(perSeg),
            thenHoldForDuration: 0
        )
        prev = next
    }
}

// TRIAL: prevent the synthesize failure (which ends the test) by typing into
// the element that actually HAS keyboard focus, rather than a no-arg
// XCUIApplication() (crashes) or a blind app.typeText (fails the test if no
// field is focused). A focused element that exists can't raise the synthesize
// failure. If nothing has focus, return a clean error and keep serving.
private func act_type(_ j: [String: Any]) throws {
    guard let text = j["text"] as? String else { throw ActError.missing("text") }
    // The focused field can live in different processes: the caller's app, the
    // Spotlight UI, or springboard's home-screen search. Search each for an
    // element that has keyboard focus and type into the first one found.
    var candidates = ["com.apple.Spotlight", "com.apple.springboard"]
    if let b = j["bundle_id"] as? String { candidates.insert(b, at: 0) }
    let focusPredicate = NSPredicate(format: "hasKeyboardFocus == true")
    for bundleId in candidates {
        let focused = XCUIApplication(bundleIdentifier: bundleId)
            .descendants(matching: .any).matching(focusPredicate).firstMatch
        if focused.exists {
            focused.typeText(text)
            return
        }
    }
    throw ActError.bad("no text field has keyboard focus")
}

private func act_button(_ j: [String: Any]) throws {
    guard let name = j["button"] as? String else { throw ActError.missing("button") }
    switch name {
    case "home": XCUIDevice.shared.press(.home)
    default: throw ActError.bad("unsupported button: \(name) — only 'home' is wired in v0.6.0")
    }
}

private func act_orientation(_ j: [String: Any]) throws {
    guard let name = j["orientation"] as? String else { throw ActError.missing("orientation") }
    let o: UIDeviceOrientation
    switch name {
    case "portrait": o = .portrait
    case "portrait_upside_down": o = .portraitUpsideDown
    case "landscape_left": o = .landscapeLeft
    case "landscape_right": o = .landscapeRight
    default: throw ActError.bad("unsupported orientation: \(name)")
    }
    // Reset through portrait first when changing landscape<->landscape; clears
    // the stuck-orientation state XCUIDevice accumulates (WDA workaround).
    if XCUIDevice.shared.orientation != .portrait && o != .portrait {
        XCUIDevice.shared.orientation = .portrait
        Thread.sleep(forTimeInterval: 0.3)
    }
    XCUIDevice.shared.orientation = o
}

// MARK: - Tree hint (best-effort, null on failure)

private func treeHint(bundle: String?) -> [String: Any]? {
    // Snapshot the FOREGROUND app (so its real elements are visible) merged
    // with SpringBoard (so system alerts + keyboard stay in the tree). Falls
    // back to SpringBoard alone when the caller doesn't name the app — which is
    // why element-finding saw nothing but SpringBoard before this fix.
    var children: [[String: Any]] = []
    if let b = bundle, !b.isEmpty, b != "com.apple.springboard" {
        if let app = try? XCUIApplication(bundleIdentifier: b).snapshot() {
            children.append(serialize(app))
        }
    }
    if let sb = try? springboard().snapshot() {
        children.append(serialize(sb))
    }
    switch children.count {
    case 0: return nil
    case 1: return children[0]
    default: return ["label": "", "type": "Application", "frame": [0, 0, 0, 0], "children": children]
    }
}

/// Downscale the 3× retina screenshot to ~2× and JPEG-compress it: ~10× smaller
/// than the raw PNG with negligible vision loss, and fewer image tokens for the
/// agent. Falls back to PNG if JPEG encoding ever fails.
private func encodeScreenshot(_ image: UIImage) -> Data {
    let cappedScale = min(image.scale, 2.0)
    let fmt = UIGraphicsImageRendererFormat.default()
    fmt.scale = cappedScale
    fmt.opaque = true
    let renderer = UIGraphicsImageRenderer(size: image.size, format: fmt)
    let scaled = renderer.image { _ in
        image.draw(in: CGRect(origin: .zero, size: image.size))
    }
    return scaled.jpegData(compressionQuality: 0.6) ?? image.pngData() ?? Data()
}

private func serialize(_ s: XCUIElementSnapshot) -> [String: Any] {
    var d: [String: Any] = [
        "type": elementTypeName(s.elementType),
        "frame": [Int(s.frame.origin.x), Int(s.frame.origin.y), Int(s.frame.width), Int(s.frame.height)],
    ]
    if !s.identifier.isEmpty { d["id"] = s.identifier }
    if !s.label.isEmpty { d["label"] = s.label }
    if let v = s.value as? String, !v.isEmpty { d["value"] = v }
    if s.isEnabled { d["enabled"] = true }
    let kids = s.children.map(serialize)
    if !kids.isEmpty { d["children"] = kids }
    return d
}

private func elementTypeName(_ t: XCUIElement.ElementType) -> String {
    switch t {
    case .application: return "Application"
    case .window: return "Window"
    case .button: return "Button"
    case .staticText: return "StaticText"
    case .textField: return "TextField"
    case .secureTextField: return "SecureTextField"
    case .image: return "Image"
    case .cell: return "Cell"
    case .table: return "Table"
    case .scrollView: return "ScrollView"
    case .navigationBar: return "NavigationBar"
    case .toolbar: return "Toolbar"
    case .tabBar: return "TabBar"
    case .link: return "Link"
    case .switch: return "Switch"
    case .slider: return "Slider"
    case .picker: return "Picker"
    case .pickerWheel: return "PickerWheel"
    case .alert: return "Alert"
    case .sheet: return "Sheet"
    case .keyboard: return "Keyboard"
    case .key: return "Key"
    case .map: return "Map"
    case .webView: return "WebView"
    case .other: return "Other"
    default: return "Type\(t.rawValue)"
    }
}

// MARK: - HTTP server (loopback-only)

final class HttpServer {
    typealias Handler = (HttpRequest) throws -> HttpResponse
    private var listener: NWListener?
    private var routes: [(String, String, Handler)] = []
    private let queue = DispatchQueue(label: "drengr.runner.http")
    private let port: UInt16

    init(port: UInt16) throws {
        guard NWEndpoint.Port(rawValue: port) != nil else { throw HttpError.invalidPort(port) }
        self.port = port
    }

    func get(_ path: String, _ handler: @escaping Handler) { routes.append(("GET", path, handler)) }
    func post(_ path: String, _ handler: @escaping Handler) { routes.append(("POST", path, handler)) }

    func start() throws {
        try openListener()
    }

    // (Re)open the listening socket. iOS hangs up the listening socket when the
    // runner becomes suspension-eligible — which SpringBoard triggers whenever an
    // overlay takes the screen (Spotlight) or the device rotates. Without a
    // restart the NWListener stays dead and every later request gets "connection
    // refused". We detect .failed/.cancelled and re-open with a small backoff.
    // (Apple DTS guidance: close/reopen listeners around suspension eligibility.)
    private func openListener() throws {
        let params = NWParameters.tcp
        params.allowLocalEndpointReuse = true
        params.requiredInterfaceType = .loopback
        guard let nwPort = NWEndpoint.Port(rawValue: port) else { throw HttpError.invalidPort(port) }
        let l = try NWListener(using: params, on: nwPort)
        l.stateUpdateHandler = { [weak self] state in
            guard let self = self else { return }
            switch state {
            case .failed, .cancelled:
                self.queue.asyncAfter(deadline: .now() + 0.3) { try? self.openListener() }
            default:
                break
            }
        }
        l.newConnectionHandler = { [weak self] conn in self?.accept(conn) }
        l.start(queue: queue)
        self.listener = l
    }

    private func accept(_ conn: NWConnection) {
        conn.start(queue: queue)
        read(conn, buffer: Data())
    }

    private func read(_ conn: NWConnection, buffer: Data) {
        conn.receive(minimumIncompleteLength: 1, maximumLength: 1_048_576) { [weak self] data, _, isComplete, error in
            guard let self = self else { return }
            var buf = buffer
            if let data = data { buf.append(data) }
            if buf.count > MAX_REQUEST_BYTES {
                self.send(.tooLarge(), on: conn)
                return
            }
            if let request = HttpRequest.parse(buf) {
                let response = self.dispatch(request)
                self.send(response, on: conn)
                return
            }
            if error != nil || isComplete {
                self.send(.badRequest("incomplete request"), on: conn)
                return
            }
            self.read(conn, buffer: buf)
        }
    }

    private func dispatch(_ req: HttpRequest) -> HttpResponse {
        for (method, path, handler) in routes where method == req.method && path == req.path {
            do { return try handler(req) } catch { return .serverError("\(error)") }
        }
        return .notFound("no route for \(req.method) \(req.path)")
    }

    private func send(_ resp: HttpResponse, on conn: NWConnection) {
        let head = "HTTP/1.1 \(resp.status) \(statusText(resp.status))\r\nContent-Type: \(resp.contentType)\r\nContent-Length: \(resp.body.count)\r\nConnection: close\r\n\r\n"
        var bytes = head.data(using: .utf8)!
        bytes.append(resp.body)
        conn.send(content: bytes, completion: .contentProcessed { _ in conn.cancel() })
    }

    private func statusText(_ s: Int) -> String {
        switch s {
        case 200: return "OK"
        case 400: return "Bad Request"
        case 404: return "Not Found"
        case 413: return "Payload Too Large"
        case 500: return "Internal Server Error"
        default: return "OK"
        }
    }
}

enum HttpError: Error { case invalidPort(UInt16) }

struct HttpRequest {
    let method: String
    let path: String
    let query: [String: String]
    let body: Data

    static func parse(_ data: Data) -> HttpRequest? {
        let delim = Data("\r\n\r\n".utf8)
        guard let headerEnd = data.firstRange(of: delim)?.lowerBound,
              let headerStr = String(data: data.prefix(headerEnd), encoding: .utf8) else { return nil }
        var lines = headerStr.components(separatedBy: "\r\n")
        guard !lines.isEmpty else { return nil }
        let parts = lines.removeFirst().split(separator: " ")
        guard parts.count >= 2 else { return nil }
        let method = String(parts[0])
        let rawTarget = String(parts[1])
        let targetParts = rawTarget.split(separator: "?", maxSplits: 1)
        let path = targetParts.first.map(String.init) ?? ""
        var query: [String: String] = [:]
        if targetParts.count > 1 {
            for pair in targetParts[1].split(separator: "&") {
                let kv = pair.split(separator: "=", maxSplits: 1)
                if kv.count == 2 {
                    let k = String(kv[0])
                    let v = String(kv[1]).removingPercentEncoding ?? String(kv[1])
                    query[k] = v
                }
            }
        }
        var contentLength = 0
        for line in lines {
            guard let colon = line.firstIndex(of: ":") else { continue }
            let name = line[..<colon].lowercased()
            let value = line[line.index(after: colon)...].trimmingCharacters(in: .whitespaces)
            if name == "content-length", let n = Int(value) { contentLength = n }
        }
        if contentLength > MAX_REQUEST_BYTES { return nil }
        let bodyStart = data.index(headerEnd, offsetBy: delim.count)
        let body = bodyStart < data.endIndex ? Data(data[bodyStart...]) : Data()
        guard body.count >= contentLength else { return nil }
        return HttpRequest(method: method, path: path, query: query, body: body.prefix(contentLength))
    }
}

struct HttpResponse {
    let status: Int
    let contentType: String
    let body: Data

    static func json(_ object: [String: Any]) -> HttpResponse {
        let data = (try? JSONSerialization.data(withJSONObject: object, options: [])) ?? Data("{}".utf8)
        return HttpResponse(status: 200, contentType: "application/json", body: data)
    }

    // Caller error: HTTP 200 with {ok:false}. Rust client distinguishes
    // success/failure by parsing `ok`, not by HTTP status.
    static func clientError(_ message: String) -> HttpResponse {
        let data = (try? JSONSerialization.data(withJSONObject: ["ok": false, "error": message])) ?? Data()
        return HttpResponse(status: 200, contentType: "application/json", body: data)
    }

    // Server fault: HTTP 500. Reserved for genuine runner bugs.
    static func serverError(_ message: String) -> HttpResponse {
        let data = (try? JSONSerialization.data(withJSONObject: ["ok": false, "error": message])) ?? Data()
        return HttpResponse(status: 500, contentType: "application/json", body: data)
    }

    static func notFound(_ message: String) -> HttpResponse {
        HttpResponse(status: 404, contentType: "text/plain", body: message.data(using: .utf8) ?? Data())
    }

    static func badRequest(_ message: String) -> HttpResponse {
        HttpResponse(status: 400, contentType: "text/plain", body: message.data(using: .utf8) ?? Data())
    }

    static func tooLarge() -> HttpResponse {
        HttpResponse(status: 413, contentType: "text/plain", body: Data("payload too large".utf8))
    }
}
