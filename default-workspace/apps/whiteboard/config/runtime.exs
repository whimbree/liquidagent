import Config

# The liquid run contract (ADR 0002): the supervisor allocates PORT and
# proxies /app/whiteboard/* here. Bind loopback only — the supervisor is the
# front door and enforces app visibility.
port = String.to_integer(System.get_env("PORT") || "4000")

config :whiteboard, WhiteboardWeb.Endpoint,
  http: [ip: {127, 0, 0, 1}, port: port],
  # No sessions/cookies are signed in this app; the base just has to exist.
  secret_key_base:
    System.get_env("SECRET_KEY_BASE") || String.duplicate("liquid-whiteboard-", 4),
  server: true
