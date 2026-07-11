import Config

config :whiteboard, WhiteboardWeb.Endpoint,
  adapter: Bandit.PhoenixAdapter,
  url: [path: "/"],
  render_errors: [formats: [html: WhiteboardWeb.ErrorHTML], layout: false],
  pubsub_server: Whiteboard.PubSub

config :logger, level: :info
