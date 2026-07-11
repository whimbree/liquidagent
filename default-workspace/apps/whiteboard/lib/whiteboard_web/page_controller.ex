defmodule WhiteboardWeb.PageController do
  use Phoenix.Controller, formats: []

  def index(conn, _params) do
    conn
    |> put_resp_content_type("text/html")
    |> send_file(200, Application.app_dir(:whiteboard, "priv/static/index.html"))
  end

  def health(conn, _params), do: text(conn, "ok")
end
