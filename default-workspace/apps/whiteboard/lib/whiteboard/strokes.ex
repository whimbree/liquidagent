defmodule Whiteboard.Strokes do
  @moduledoc """
  The board state: an in-memory map of board => strokes, debounce-persisted
  as JSON into the liquid per-app data dir (`LIQUID_APP_DATA_DIR`, gitignored)
  so drawings survive backend restarts and redeploys.
  """
  use GenServer

  @save_debounce_ms 1_000
  # A stroke is a small map; boards are personal-scale. Cap so an abusive
  # guest can't grow the file without bound.
  @max_strokes_per_board 20_000

  def start_link(_opts), do: GenServer.start_link(__MODULE__, nil, name: __MODULE__)

  def all(board), do: GenServer.call(__MODULE__, {:all, board})
  def add(board, stroke), do: GenServer.cast(__MODULE__, {:add, board, stroke})
  def clear(board), do: GenServer.cast(__MODULE__, {:clear, board})

  @impl true
  def init(nil) do
    boards =
      with {:ok, json} <- File.read(path()),
           {:ok, boards} when is_map(boards) <- Jason.decode(json) do
        boards
      else
        _ -> %{}
      end

    {:ok, %{boards: boards, save_scheduled: false}}
  end

  @impl true
  def handle_call({:all, board}, _from, state) do
    {:reply, state.boards |> Map.get(board, []) |> Enum.reverse(), state}
  end

  @impl true
  def handle_cast({:add, board, stroke}, state) do
    boards =
      Map.update(state.boards, board, [stroke], fn strokes ->
        Enum.take([stroke | strokes], @max_strokes_per_board)
      end)

    {:noreply, schedule_save(%{state | boards: boards})}
  end

  def handle_cast({:clear, board}, state) do
    {:noreply, schedule_save(%{state | boards: Map.delete(state.boards, board)})}
  end

  @impl true
  def handle_info(:save, state) do
    case Jason.encode(state.boards) do
      {:ok, json} -> File.write(path(), json)
      _ -> :ok
    end

    {:noreply, %{state | save_scheduled: false}}
  end

  defp schedule_save(%{save_scheduled: true} = state), do: state

  defp schedule_save(state) do
    Process.send_after(self(), :save, @save_debounce_ms)
    %{state | save_scheduled: true}
  end

  defp path do
    dir = System.get_env("LIQUID_APP_DATA_DIR") || "data"
    File.mkdir_p!(dir)
    Path.join(dir, "strokes.json")
  end
end
