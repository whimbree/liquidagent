defmodule Whiteboard.Application do
  use Application

  @impl true
  def start(_type, _args) do
    children = [
      {Phoenix.PubSub, name: Whiteboard.PubSub},
      WhiteboardWeb.Presence,
      Whiteboard.Strokes,
      WhiteboardWeb.Endpoint
    ]

    Supervisor.start_link(children, strategy: :one_for_one, name: Whiteboard.Supervisor)
  end
end
