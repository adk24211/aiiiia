const { verify } = require("./verify.js");
const SECRET = "whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw";
const ID = "msg_p5jXN8AQM9LWM0D4loKWxJek";
const TS = "1614265330";
const BODY = '{"test": 2432232314}';
const SIG = "v1,g0hM9SsE+OTPJTGt/tmIKtSyZlE3uFJELVlNIOLJ1OE=";
const h = { "webhook-id": ID, "webhook-timestamp": TS, "webhook-signature": SIG };
const huge = 1e9;
console.log("valid:", verify(SECRET, h, BODY, huge));
try { verify(SECRET, h, '{"test": 2432232315}', huge); console.log("FAIL tamper"); }
catch (e) { console.log("tampered rejected:", e.message); }
try { verify(SECRET, h, BODY, 1); console.log("FAIL stale"); }
catch (e) { console.log("stale rejected:", e.message); }
const rotating = { ...h, "webhook-signature": "v1,AAAA " + SIG };
console.log("rotation:", verify(SECRET, rotating, BODY, huge));
