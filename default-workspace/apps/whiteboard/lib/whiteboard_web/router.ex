defmodule WhiteboardWeb.Router do
  use Phoenix.Router

  get "/", WhiteboardWeb.PageController, :index
  get "/health", WhiteboardWeb.PageController, :health
end
