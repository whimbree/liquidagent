defmodule WhiteboardWeb.BoardChannel do
  @moduledoc """
  One shared board per topic ("board:main"). Joiners get the stroke history
  and a server-assigned color; strokes broadcast to everyone else and persist;
  cursors broadcast but never persist; Presence tracks who's here.
  """
  use Phoenix.Channel

  alias Whiteboard.Strokes
  alias WhiteboardWeb.Presence

  @colors ~w(#ff6b6b #ffd93d #6bcb77 #4d96ff #b980f0 #ff9f45 #f473b9 #2dd4bf)
  # A stroke segment is capped so one message can't be unbounded.
  @max_points 512

  @impl true
  def join("board:" <> board, params, socket) do
    user = params |> Map.get("user", "") |> to_string() |> String.slice(0, 32)
    user = if user == "", do: random_id(), else: user
    color = Enum.at(@colors, :erlang.phash2(user, length(@colors)))

    socket =
      socket
      |> assign(:board, board)
      |> assign(:user, user)
      |> assign(:color, color)

    send(self(), :after_join)
    {:ok, %{strokes: Strokes.all(board), color: color, user: user}, socket}
  end

  @impl true
  def handle_info(:after_join, socket) do
    {:ok, _} = Presence.track(socket, socket.assigns.user, %{color: socket.assigns.color})
    push(socket, "presence_state", Presence.list(socket))
    {:noreply, socket}
  end

  @impl true
  def handle_in("stroke", %{"points" => points} = payload, socket)
      when is_list(points) and points != [] and length(points) <= @max_points do
    stroke = %{
      "points" => points,
      "color" => socket.assigns.color,
      "user" => socket.assigns.user,
      "width" => payload |> Map.get("width", 3) |> normalize_width()
    }

    Strokes.add(socket.assigns.board, stroke)
    broadcast_from!(socket, "stroke", stroke)
    {:noreply, socket}
  end

  def handle_in("stroke", _bad, socket), do: {:noreply, socket}

  def handle_in("cursor", %{"x" => x, "y" => y}, socket) when is_number(x) and is_number(y) do
    broadcast_from!(socket, "cursor", %{
      "user" => socket.assigns.user,
      "color" => socket.assigns.color,
      "x" => x,
      "y" => y
    })

    {:noreply, socket}
  end

  def handle_in("cursor", _bad, socket), do: {:noreply, socket}

  def handle_in("clear", _payload, socket) do
    Strokes.clear(socket.assigns.board)
    broadcast!(socket, "clear", %{"user" => socket.assigns.user})
    {:noreply, socket}
  end

  defp normalize_width(w) when is_number(w), do: w |> max(1) |> min(24)
  defp normalize_width(_), do: 3

  defp random_id do
    :crypto.strong_rand_bytes(4) |> Base.encode16(case: :lower)
  end
end
