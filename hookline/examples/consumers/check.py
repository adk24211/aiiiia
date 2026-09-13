from verify import verify
S="whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw"; I="msg_p5jXN8AQM9LWM0D4loKWxJek"
T="1614265330"; B=b'{"test": 2432232314}'
SIG="v1,g0hM9SsE+OTPJTGt/tmIKtSyZlE3uFJELVlNIOLJ1OE="
h={"webhook-id":I,"webhook-timestamp":T,"webhook-signature":SIG}
HUGE=10**9
print("valid:", verify(S,h,B,HUGE))
try: verify(S,h,b'{"test": 2432232315}',HUGE); print("FAIL tamper")
except ValueError as e: print("tampered rejected:", e)
try: verify(S,h,B,1); print("FAIL stale")
except ValueError as e: print("stale rejected:", e)
print("rotation:", verify(S,{**h,"webhook-signature":"v1,AAAA "+SIG},B,HUGE))
