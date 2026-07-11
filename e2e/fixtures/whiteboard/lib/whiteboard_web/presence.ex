defmodule WhiteboardWeb.Presence do
  use Phoenix.Presence, otp_app: :whiteboard, pubsub_server: Whiteboard.PubSub
end
