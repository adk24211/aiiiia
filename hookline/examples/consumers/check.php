<?php
require __DIR__ . '/verify.php';
$s = 'whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw';
$h = ['webhook-id' => 'msg_p5jXN8AQM9LWM0D4loKWxJek',
      'webhook-timestamp' => '1614265330',
      'webhook-signature' => 'v1,g0hM9SsE+OTPJTGt/tmIKtSyZlE3uFJELVlNIOLJ1OE='];
$b = '{"test": 2432232314}';
$huge = 1000000000;
echo 'valid: ' . var_export(hookline_verify($s, $h, $b, $huge), true) . "\n";
try { hookline_verify($s, $h, '{"test": 2432232315}', $huge); echo "FAIL\n"; }
catch (RuntimeException $e) { echo 'tampered rejected: ' . $e->getMessage() . "\n"; }
try { hookline_verify($s, $h, $b, 1); echo "FAIL\n"; }
catch (RuntimeException $e) { echo 'stale rejected: ' . $e->getMessage() . "\n"; }
$rot = $h; $rot['webhook-signature'] = 'v1,AAAA ' . $h['webhook-signature'];
echo 'rotation: ' . var_export(hookline_verify($s, $rot, $b, $huge), true) . "\n";
