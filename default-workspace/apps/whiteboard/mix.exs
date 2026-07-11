defmodule Whiteboard.MixProject do
  use Mix.Project

  def project do
    [
      app: :whiteboard,
      version: "0.1.0",
      elixir: "~> 1.15",
      start_permanent: Mix.env() == :prod,
      deps: deps()
    ]
  end

  def application do
    [mod: {Whiteboard.Application, []}, extra_applications: [:logger]]
  end

  # Channels + Presence only — no LiveView, no asset pipeline, no ecto.
  # deps/ is vendored (committed) so the liquid pipeline can boot this app
  # without network access.
  defp deps do
    [
      {:phoenix, "~> 1.7"},
      {:bandit, "~> 1.5"},
      {:jason, "~> 1.4"}
    ]
  end
end
