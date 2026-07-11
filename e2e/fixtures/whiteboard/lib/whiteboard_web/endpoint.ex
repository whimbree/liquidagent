defmodule WhiteboardWeb.Endpoint do
  use Phoenix.Endpoint, otp_app: :whiteboard

  # The browser reaches this at /app/whiteboard/socket/websocket; liquid's
  # proxy strips the app prefix and splices the upgraded stream through.
  # check_origin off: the app is framed behind liquid on whatever host the
  # owner serves it from — liquid's visibility rule is the access control.
  socket "/socket", WhiteboardWeb.UserSocket,
    websocket: [check_origin: false],
    longpoll: false

  plug Plug.Static, at: "/", from: {:whiteboard, "priv/static"}, gzip: false
  plug WhiteboardWeb.Router
end
