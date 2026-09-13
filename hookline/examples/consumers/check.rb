require_relative "verify"
S = "whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw"
H = { "webhook-id" => "msg_p5jXN8AQM9LWM0D4loKWxJek",
      "webhook-timestamp" => "1614265330",
      "webhook-signature" => "v1,g0hM9SsE+OTPJTGt/tmIKtSyZlE3uFJELVlNIOLJ1OE=" }
B = '{"test": 2432232314}'
HUGE = 10**9
puts "valid: #{Hookline.verify(S, H, B, tolerance_seconds: HUGE)}"
begin; Hookline.verify(S, H, '{"test": 2432232315}', tolerance_seconds: HUGE); puts "FAIL"
rescue Hookline::Invalid => e; puts "tampered rejected: #{e.message}"; end
begin; Hookline.verify(S, H, B, tolerance_seconds: 1); puts "FAIL"
rescue Hookline::Invalid => e; puts "stale rejected: #{e.message}"; end
rot = H.merge("webhook-signature" => "v1,AAAA " + H["webhook-signature"])
puts "rotation: #{Hookline.verify(S, rot, B, tolerance_seconds: HUGE)}"
