require "base64"
require "openssl"

module Hookline
  Invalid = Class.new(StandardError)

  # Verify a hookline (Standard Webhooks) signature.
  #
  # +body+ must be the raw request bytes. In Rails that is
  # +request.raw_post+, not +params+: a hash that has been parsed and dumped
  # again is different bytes and will not match.
  def self.verify(secret, headers, body, tolerance_seconds: 300)
    id        = headers["webhook-id"]
    timestamp = headers["webhook-timestamp"]
    signature = headers["webhook-signature"]
    raise Invalid, "missing webhook headers" unless id && timestamp && signature

    raise Invalid, "stale timestamp" if (Time.now.to_i - timestamp.to_i).abs > tolerance_seconds

    key = Base64.decode64(secret.delete_prefix("whsec_"))
    signed = "#{id}.#{timestamp}.#{body}"
    expected = OpenSSL::HMAC.digest("SHA256", key, signed)

    # Several space-separated signatures during a rotation; one is enough.
    signature.split(" ").each do |part|
      version, value = part.split(",", 2)
      next unless version == "v1" && value

      given = Base64.decode64(value)
      return true if given.bytesize == expected.bytesize &&
                     OpenSSL.secure_compare(given, expected)
    end
    raise Invalid, "no signature matched"
  end
end
