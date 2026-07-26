// tictactoe.st — a tic-tac-toe board, win detection, and a minimax AI.
// Board cells are `0..8`, row-major:
//
//   0 1 2
//   3 4 5
//   6 7 8
//
// **Deliberately not tested from an empty board.** `bestMove` on an empty
// board explores the full game tree (up to `9!` leaf paths) — cheap for a
// real interpreter, but `test_runner::DEFAULT_FUEL` budgets 1,000,000
// *evaluation steps* per test, and each game-tree node costs several
// (`match`, list operations, comparisons). A full-board search risks the
// budget for no real benefit: minimax's correctness is exactly as visible,
// and far cheaper to check, from small near-endgame positions (5 or fewer
// empty cells) — "does it take the winning move," "does it block the only
// threat" — which is what every test below does. Verified against an
// independent Python re-implementation before writing the `.st` (same
// habit as vm.st's/markov.st's/diff.st's entries in
// plans/stitch-examples-findings.md): a *forced-loss* board (two live
// winning threats for the opponent at once) was the first draft of the
// "must block" test, caught only because the simulator's answer didn't
// match the intended scenario.

use List

sum Cell = Empty | X | O

prod Board(cells: List<Cell>)

newBoard() -> Board = Board(map(0..=8, _ -> Empty) |> toList)

at(board: Board, i: Int) -> Cell = List.at(board.cells, i) |> unwrapOr(Empty)

place(board: Board, i: Int, mark: Cell) -> Board = Board(List.set(board.cells, i, mark))

opponent(mark: Cell) -> Cell = mark == X => O | X

winLines() -> List<List<Int>> =
    [[0, 1, 2], [3, 4, 5], [6, 7, 8], [0, 3, 6], [1, 4, 7], [2, 5, 8], [0, 4, 8], [2, 4, 6]]

lineWinner(board: Board, line: List<Int>) -> List<Cell> = {
    let marks = map(line, i -> at(board, i))
    let m = List.at(marks, 0) |> unwrapOr(Empty)
    // No parens needed around the condition (`and` already binds tighter
    // than `=>`) — and critically, a statement starting with a bare `(`
    // right after a `|>`-ending `let` fuses with the pipe's call instead
    // of starting a new statement (maximal munch again — see
    // plans/stitch-examples-findings.md). Dropping the parens sidesteps it.
    m != Empty and all(marks, c -> c == m) => [m] | []
}

winner(board: Board) -> Maybe<Cell> = winLines() |> flatMap(line -> lineWinner(board, line)) |> first

isFull(board: Board) -> Bool = not contains(board.cells, Empty)

gameOver(board: Board) -> Bool = isSome(winner(board)) or isFull(board)

emptyIndices(board: Board) -> List<Int> =
    enumerate(board.cells)
        |> filter(pair -> match pair { (i, c) => c == Empty })
        |> map(pair -> match pair { (i, c) => i })

// +1 if X has (or forces) a win, -1 for O, 0 for a drawn-out game — the
// standard minimax value, computed by trying every legal move for
// `player` and recursing, maximizing at X's turns and minimizing at O's.
minimax(board: Board, player: Cell) -> Int =
    match winner(board) {
        Some(X) => 1
        Some(O) => 0 - 1
        Some(_) => 0
        None =>
            isFull(board)
                => 0
                | {
                    let scores = map(emptyIndices(board), i -> minimax(place(board, i, player), opponent(player)))
                    player == X => maxOf(scores) | minOf(scores)
                }
    }

maxOf(xs: List<Int>) -> Int = max(xs) |> unwrapOr(0)
minOf(xs: List<Int>) -> Int = min(xs) |> unwrapOr(0)

prod ScoredMove(index: Int, score: Int)

// The move `player` should make: the one whose resulting position's
// minimax value is best *for `player`* (highest for X, lowest for O).
bestMove(board: Board, player: Cell) -> Maybe<Int> =
    emptyIndices(board)
        |> map(i -> ScoredMove(i, minimax(place(board, i, player), opponent(player))))
        |> pickBest(player)
        |> mapMaybe(sm -> sm.index)

pickBest(scored: List<ScoredMove>, player: Cell) -> Maybe<ScoredMove> =
    fold(scored, None, (best, sm) -> match best {
        None => Some(sm)
        Some(b) => (player == X => sm.score > b.score | sm.score < b.score) => Some(sm) | best
    })

// --- tests ---

test "winner detects a row, a column, and a diagonal" {
    let row = Board([X, X, X, Empty, Empty, Empty, Empty, Empty, Empty])
    let col = Board([O, Empty, Empty, O, Empty, Empty, O, Empty, Empty])
    let diag = Board([X, Empty, Empty, Empty, X, Empty, Empty, Empty, X])
    expect winner(row) == Some(X)
    expect winner(col) == Some(O)
    expect winner(diag) == Some(X)
}

test "a board with no three in a row has no winner" {
    expect winner(newBoard()) == None
    expect winner(Board([X, O, X, X, O, O, O, X, X])) == None
}

test "isFull and gameOver recognize a completed draw" {
    let draw = Board([X, O, X, X, O, O, O, X, O])
    expect winner(draw) == None
    expect isFull(draw) == true
    expect gameOver(draw) == true
}

test "minimax scores a position X has already won as +1" {
    let won = Board([X, X, X, O, O, Empty, Empty, Empty, Empty])
    expect minimax(won, O) == 1
}

test "minimax scores a drawn-out full board as 0" {
    let draw = Board([X, O, X, X, O, O, O, X, O])
    expect minimax(draw, X) == 0
}

test "bestMove takes the immediate winning move when one exists" {
    // X X _   O O _
    let board = Board([X, X, Empty, O, O, Empty, Empty, Empty, Empty])
    expect bestMove(board, X) == Some(2)
}

test "bestMove blocks the opponent's sole winning threat" {
    // X _ _
    // O O _   <- O wins at 5 unless blocked
    // X _ _
    let board = Board([X, Empty, Empty, O, O, Empty, X, Empty, Empty])
    expect bestMove(board, X) == Some(5)
}

test "bestMove on a nearly-full board just plays the only cell left" {
    let board = Board([X, O, X, X, O, O, O, X, Empty])
    expect bestMove(board, X) == Some(8)
}
