// Tetris - all seven standard pieces, 4-way rotation (simplified wall-kick, no full SRS
// kick table), 7-bag randomization, a score counter, a side-mounted HUD (score + next-piece
// preview), and a flash animation for row clears.

use core::fmt::Write;
use std::collections::VecDeque;

use blitstr2::GlyphStyle;
use ux_api::minigfx::*;
use ux_api::service::api::Gid;
use ux_api::service::gfx::Gfx;

const BOARD_W: usize = 10;
// board height is sized at construction time
const MIN_BOARD_H: usize = 16;
const MAX_BOARD_H: usize = 24;

// reservation on left side for next piece/score
const HUD_WIDTH: isize = 34;

// padding for outline
const FRAME_PAD: isize = 3;

// main.rs fires tetris ticks at 10ms, one gravity tick should be 600ms, so 10 * 60
const GRAVITY_TICKS: u32 = 60;

// timing for flash animation on clear
const CLEAR_ANIM_TICKS_PER_FRAME: u8 = 8;
const CLEAR_ANIM_FRAMES: u8 = 4;

// how often idle_tick() takes a solver step while idle mode is on, in raw ticks
// (30 * 10ms = 300ms - half the speed of the original 15-tick/150ms pace)
const IDLE_MOVE_TICKS: u32 = 30;

#[derive(Clone, Copy, PartialEq)]
enum Piece {
    O,
    I,
    S,
    Z,
    T,
    L,
    J,
}

const ALL_PIECES: [Piece; 7] =
    [Piece::O, Piece::I, Piece::S, Piece::Z, Piece::T, Piece::L, Piece::J];

struct ClearAnim {
    rows: Vec<usize>,
    frame: u8,
    // waw ticks elapsed within the current frame/phase
    tick: u8,
}

pub struct TetrisGame {
    board: Vec<[bool; BOARD_W]>,
    board_h: usize,
    piece: Piece,
    // rotation state 0..=3 (spawn, 90° CW, 180°, 270° CW)
    rotation: u8,
    px: i32,
    py: i32,
    // 7-bag queue: pieces are drawn from the front, and a fresh shuffled set of all seven is
    // appended whenever fewer than 2 remain 
    queue: VecDeque<Piece>,
    next_piece: Piece,
    score: u32,
    clearing: Option<ClearAnim>,
    // raw ticks elapsed since the last gravity step
    gravity_accum: u32,
    game_over: bool,
    // set once MenuTetris has spun up the game; gates the '↑' key from toggling idle mode
    // before the game is actually up and running
    pub(crate) started: bool,
    // true while the "playing itself" idle animation is active - see idle_tick
    idle_mode: bool,
    // raw ticks since the last idle move
    idle_accum: u32,
    cell_px: isize,
    origin_x: isize,
    origin_y: isize,
}

impl TetrisGame {
    pub fn new(gfx: &Gfx) -> Self {
        let screen = gfx.screen_size().unwrap();
        // post-hud usable width
        let usable_w = (screen.x - HUD_WIDTH).max(BOARD_W as isize);
        // post-padding usable height
        let usable_h = (screen.y - 2 * FRAME_PAD).max(MIN_BOARD_H as isize);

        let cell_px_by_width = (usable_w / BOARD_W as isize).max(1);
        let rows_that_fit = (usable_h / cell_px_by_width).max(0) as usize;

        let (cell_px, board_h) = if rows_that_fit >= MIN_BOARD_H {
            // normally width-driven cell size
            (cell_px_by_width, rows_that_fit.min(MAX_BOARD_H))
        } else {
            // height driven cell size given unusually short dimensions
            let cell_px_by_height = (usable_h / MIN_BOARD_H as isize).max(1);
            (cell_px_by_height.min(cell_px_by_width), MIN_BOARD_H)
        };

        let board_w_px = cell_px * BOARD_W as isize;
        let board_h_px = cell_px * board_h as isize;
        let origin_x = (usable_w - board_w_px) / 2;
        let origin_y = (screen.y - board_h_px) / 2;

        let mut game = TetrisGame {
            board: vec![[false; BOARD_W]; board_h],
            board_h,
            // placeholders - both immediately overwritten by spawn() below
            piece: Piece::O,
            next_piece: Piece::O,
            rotation: 0,
            px: 0,
            py: 0,
            queue: VecDeque::new(),
            score: 0,
            clearing: None,
            gravity_accum: 0,
            game_over: false,
            started: false,
            idle_mode: false,
            idle_accum: 0,
            cell_px,
            origin_x,
            origin_y,
        };
        game.spawn();
        game
    }

    fn cell_rect(&self, col: i32, row: i32, filled: bool) -> RoundedRectangle {
        let x0 = self.origin_x + col as isize * self.cell_px;
        let y0 = self.origin_y + row as isize * self.cell_px;
        let fill = if filled { PixelColor::Dark } else { PixelColor::Light };
        RoundedRectangle {
            border: Rectangle {
                tl: Point::new(x0, y0),
                br: Point::new(x0 + self.cell_px - 1, y0 + self.cell_px - 1),
                style: DrawStyle::new(PixelColor::Dark, fill, 1),
            },
            radius: 2,
        }
    }

    pub fn is_over(&self) -> bool {
        self.game_over
    }

    // standard tetromino rotation states, each expressed as 4 (col, row) offsets within a
    // bounding box (2x2 for O, 4x4 for I, 3x3 for the rest).
    fn cells_for(piece: Piece, rotation: u8) -> [(i32, i32); 4] {
        match piece {
            Piece::O => [(0, 0), (1, 0), (0, 1), (1, 1)],
            Piece::I => match rotation {
                0 => [(0, 1), (1, 1), (2, 1), (3, 1)],
                1 => [(2, 0), (2, 1), (2, 2), (2, 3)],
                2 => [(0, 2), (1, 2), (2, 2), (3, 2)],
                _ => [(1, 0), (1, 1), (1, 2), (1, 3)],
            },
            Piece::S => match rotation {
                0 => [(1, 0), (2, 0), (0, 1), (1, 1)],
                1 => [(1, 0), (1, 1), (2, 1), (2, 2)],
                2 => [(1, 1), (2, 1), (0, 2), (1, 2)],
                _ => [(0, 0), (0, 1), (1, 1), (1, 2)],
            },
            Piece::Z => match rotation {
                0 => [(0, 0), (1, 0), (1, 1), (2, 1)],
                1 => [(2, 0), (1, 1), (2, 1), (1, 2)],
                2 => [(0, 1), (1, 1), (1, 2), (2, 2)],
                _ => [(1, 0), (0, 1), (1, 1), (0, 2)],
            },
            Piece::T => match rotation {
                0 => [(1, 0), (0, 1), (1, 1), (2, 1)],
                1 => [(1, 0), (1, 1), (2, 1), (1, 2)],
                2 => [(0, 1), (1, 1), (2, 1), (1, 2)],
                _ => [(1, 0), (0, 1), (1, 1), (1, 2)],
            },
            Piece::L => match rotation {
                0 => [(2, 0), (0, 1), (1, 1), (2, 1)],
                1 => [(1, 0), (1, 1), (1, 2), (2, 2)],
                2 => [(0, 1), (1, 1), (2, 1), (0, 2)],
                _ => [(0, 0), (1, 0), (1, 1), (1, 2)],
            },
            Piece::J => match rotation {
                0 => [(0, 0), (0, 1), (1, 1), (2, 1)],
                1 => [(1, 0), (2, 0), (1, 1), (1, 2)],
                2 => [(0, 1), (1, 1), (2, 1), (2, 2)],
                _ => [(1, 0), (1, 1), (0, 2), (1, 2)],
            },
        }
    }

    fn cells(&self) -> [(i32, i32); 4] {
        Self::cells_for(self.piece, self.rotation)
    }

    fn collides_shape(&self, px: i32, py: i32, cells: [(i32, i32); 4]) -> bool {
        for (dx, dy) in cells {
            let x = px + dx;
            let y = py + dy;
            if x < 0 || x >= BOARD_W as i32 || y >= self.board_h as i32 {
                return true;
            }
            if y >= 0 && self.board[y as usize][x as usize] {
                return true;
            }
        }
        false
    }

    fn collides(&self, px: i32, py: i32) -> bool {
        self.collides_shape(px, py, self.cells())
    }

    // toggles idle animation mode - see the `idle_mode` field
    pub fn toggle_idle_mode(&mut self) {
        self.idle_mode = !self.idle_mode;
        self.idle_accum = 0;
    }

    pub fn is_idle_mode(&self) -> bool {
        self.idle_mode
    }

    // called once per raw tick like gravity_tick - while idle_mode is on, every
    // IDLE_MOVE_TICKS ticks this takes one step (rotate/slide/drop) toward the current best
    // placement, as picked by the heuristic solver below. Returns whether anything changed,
    // so the caller knows to redraw.
    pub fn idle_tick(&mut self) -> bool {
        if !self.idle_mode || self.game_over || self.clearing.is_some() {
            return false;
        }
        self.idle_accum += 1;
        if self.idle_accum < IDLE_MOVE_TICKS {
            return false;
        }
        self.idle_accum = 0;

        let before = (self.px, self.py, self.rotation);
        self.bot_tick();
        (self.px, self.py, self.rotation) != before
    }

    // heuristic solver: picks a placement for the current piece and walks toward it
    // simulates dropping a piece (given as 4 (dx,dy) cells)
    fn simulate_drop(
        board: &[[bool; BOARD_W]],
        board_h: usize,
        px: i32,
        cells: [(i32, i32); 4],
    ) -> Option<Vec<[bool; BOARD_W]>> {
        let collides = |px: i32, py: i32| -> bool {
            for (dx, dy) in cells {
                let (x, y) = (px + dx, py + dy);
                if x < 0 || x >= BOARD_W as i32 || y >= board_h as i32 {
                    return true;
                }
                if y >= 0 && board[y as usize][x as usize] {
                    return true;
                }
            }
            false
        };
        if collides(px, 0) {
            return None;
        }
        let mut py = 0;
        let mut iterations = 0;
        while !collides(px, py + 1) {
            py += 1;
            iterations += 1;
            if iterations > 1000 {  // board should never be this tall
                panic!("simulate_drop infinite loop: px={}, cells={:?}", px, cells);
            }
        }
        let mut new_board = board.to_vec();
        for (dx, dy) in cells {
            let (x, y) = (px + dx, py + dy);
            if y >= 0 {
                new_board[y as usize][x as usize] = true;
            }
        }
        Some(new_board)
    }

    // classic 4-term heuristic: higher is better, standard published starting weights
    fn score_board(board: &[[bool; BOARD_W]], board_h: usize) -> f32 {
        let mut heights = [0i32; BOARD_W];
        for col in 0..BOARD_W {
            heights[col] =
                (0..board_h).find(|&r| board[r][col]).map_or(0, |r| (board_h - r) as i32);
        }
        let agg_height: i32 = heights.iter().sum();
        let bumpiness: i32 = heights.windows(2).map(|w| (w[0] - w[1]).abs()).sum();

        let mut holes = 0;
        for col in 0..BOARD_W {
            let mut seen_block = false;
            for row in 0..board_h {
                if board[row][col] {
                    seen_block = true;
                } else if seen_block {
                    holes += 1;
                }
            }
        }
        let lines_cleared = (0..board_h).filter(|&r| board[r].iter().all(|&c| c)).count() as i32;

        -0.51 * agg_height as f32 - 0.36 * holes as f32 - 0.18 * bumpiness as f32
            + 0.76 * lines_cleared as f32
    }

    // tries every (rotation, column) placement for the current piece, returns the
    // best-scoring one
    fn best_placement(&self) -> Option<(u8, i32)> {
        let mut best: Option<(u8, i32, f32)> = None;
        let rotations: &[u8] = if self.piece == Piece::O { &[0] } else { &[0, 1, 2, 3] };
        for &rotation in rotations {
            let cells = Self::cells_for(self.piece, rotation);
            for col in -3..(BOARD_W as i32 + 3) {
                if let Some(board) = Self::simulate_drop(&self.board, self.board_h, col, cells) {
                    let score = Self::score_board(&board, self.board_h);
                    if best.map_or(true, |(_, _, s)| score > s) {
                        best = Some((rotation, col, score));
                    }
                }
            }
        }
        best.map(|(r, c, _)| (r, c))
    }

    // one step (rotate first, then slide, then hard-drop once aligned) toward the current
    // best placement, recomputes the target every call
    fn bot_tick(&mut self) {
        let (target_rotation, target_col) = match self.best_placement() {
            Some(t) => t,
            None => return, // shouldn't happen - spawn already checked for game_over
        };
        if self.rotation != target_rotation {
            let rotation_before = self.rotation;
            self.rotate();
            if self.rotation != rotation_before {
                return;
            }
            // rotate() failed at this column (e.g. wall-kick range exhausted near an edge) -
            // slide toward the target column instead of retrying the same rotation forever,
            // then rotation will be re-attempted from a more favorable column next tick
            if self.px < target_col {
                self.move_right();
            } else if self.px > target_col {
                self.move_left();
            }
            // if we're already at target_col and rotation still won't succeed, best_placement
            // will simply be re-evaluated fresh next tick against whatever the board allows
        } else if self.px < target_col {
            self.move_right();
        } else if self.px > target_col {
            self.move_left();
        } else {
            self.hard_drop();
        }
    }

    // attempt simple wall-kick
    pub fn rotate(&mut self) {
        if self.game_over || self.clearing.is_some() || self.piece == Piece::O {
            return;
        }
        let new_rotation = (self.rotation + 1) % 4;
        let new_cells = Self::cells_for(self.piece, new_rotation);
        // widened from [0, -1, 1, -2, 2] - the 4-wide I-piece bounding box can need a 3-cell
        // kick near the walls, and the narrower table let it get stuck failing to rotate there
        for kick in [0, -1, 1, -2, 2, -3, 3] {
            if !self.collides_shape(self.px + kick, self.py, new_cells) {
                self.px += kick;
                self.rotation = new_rotation;
                return;
            }
        }
        // no valid position found - leave orientation unchanged
    }

    // lock currenct piece, check for clears
    fn lock(&mut self) {
        for (dx, dy) in self.cells() {
            let x = self.px + dx;
            let y = self.py + dy;
            if y >= 0 && y < self.board_h as i32 {
                self.board[y as usize][x as usize] = true;
            }
        }
        let full_rows: Vec<usize> =
            (0..self.board_h).filter(|&r| self.board[r].iter().all(|&c| c)).collect();
        if full_rows.is_empty() {
            self.spawn();
        } else {
            self.score += Self::score_for(full_rows.len() as u32);
            self.clearing = Some(ClearAnim { rows: full_rows, frame: 0, tick: 0 });
        }
    }

    // level-less line-clear scoring (single/double/triple/tetris)
    fn score_for(lines_cleared: u32) -> u32 {
        match lines_cleared {
            1 => 100,
            2 => 300,
            3 => 500,
            4 => 800,
            _ => 0,
        }
    }

    // collapses whatever rows were flagged by `lock()`, rebuilding the board bottom-up
    fn finish_clear(&mut self) {
        if let Some(anim) = self.clearing.take() {
            let mut new_board = vec![[false; BOARD_W]; self.board_h];
            let mut write_row = self.board_h as i32 - 1;
            for row in (0..self.board_h).rev() {
                if !anim.rows.contains(&row) {
                    new_board[write_row as usize] = self.board[row];
                    write_row -= 1;
                }
            }
            self.board = new_board;
        }
        self.spawn();
    }

    // tops up the 7-bag queue whenever fewer than 2 pieces remain in it
    fn ensure_queue(queue: &mut VecDeque<Piece>) {
        while queue.len() < 2 {
            let mut bag = ALL_PIECES;
            // Fisher-Yates shuffle, uses RngCore::next_u32() directly
            use rand::RngCore;
            let mut rng = rand::thread_rng();
            for i in (1..bag.len()).rev() {
                let j = (rng.next_u32() as usize) % (i + 1);
                bag.swap(i, j);
            }
            queue.extend(bag);
        }
    }

    fn spawn(&mut self) {
        Self::ensure_queue(&mut self.queue);
        self.piece = self.queue.pop_front().expect("queue was just topped up");
        Self::ensure_queue(&mut self.queue);
        self.next_piece = *self.queue.front().expect("queue was just topped up");
        self.rotation = 0;
        self.px = BOARD_W as i32 / 2 - 1;
        self.py = 0;
        self.gravity_accum = 0;
        if self.collides(self.px, self.py) {
            self.game_over = true;
        }
    }

    pub fn move_left(&mut self) {
        if !self.game_over && self.clearing.is_none() && !self.collides(self.px - 1, self.py) {
            self.px -= 1;
        }
    }

    pub fn move_right(&mut self) {
        if !self.game_over && self.clearing.is_none() && !self.collides(self.px + 1, self.py) {
            self.px += 1;
        }
    }

    pub fn hard_drop(&mut self) {
        if self.game_over || self.clearing.is_some() {
            return;
        }
        while !self.collides(self.px, self.py + 1) {
            self.py += 1;
        }
        self.lock();
    }

    // called on every raw tick from main.rs's fast (10ms) timer thread, most calls are no-ops
    // visually - this just counts ticks until it's time to either step gravity or flash
    pub fn gravity_tick(&mut self) -> bool {
        if self.game_over {
            return false;
        }

        if let Some(anim) = self.clearing.as_mut() {
            anim.tick += 1;
            if anim.tick < CLEAR_ANIM_TICKS_PER_FRAME {
                return false;
            }
            anim.tick = 0;
            anim.frame += 1;
            if anim.frame >= CLEAR_ANIM_FRAMES {
                self.finish_clear();
            }
            return true;
        }

        self.gravity_accum += 1;
        if self.gravity_accum < GRAVITY_TICKS {
            return false;
        }
        self.gravity_accum = 0;

        if !self.collides(self.px, self.py + 1) {
            self.py += 1;
        } else {
            self.lock();
        }
        true
    }

    pub fn draw(&self, gfx: &Gfx) {
        gfx.clear().ok();

        self.draw_board_frame(gfx);

        let flash_on = self.clearing.as_ref().map_or(false, |a| a.frame % 2 == 0);
        for row in 0..self.board_h {
            let is_clearing_row = self.clearing.as_ref().map_or(false, |a| a.rows.contains(&row));
            for col in 0..BOARD_W {
                if is_clearing_row {
                    // flash: solid highlight on "on" frames, plain outline on "off" frames
                    gfx.draw_rounded_rectangle(self.cell_rect(col as i32, row as i32, flash_on)).ok();
                } else if self.board[row][col] {
                    gfx.draw_rounded_rectangle(self.cell_rect(col as i32, row as i32, false)).ok();
                }
            }
        }

        if !self.game_over && self.clearing.is_none() {
            for (dx, dy) in self.cells() {
                let x = self.px + dx;
                let y = self.py + dy;
                if y >= 0 {
                    gfx.draw_rounded_rectangle(self.cell_rect(x, y, false)).ok();
                }
            }
        }

        self.draw_hud(gfx);

        gfx.flush().ok();
    }

    fn draw_board_frame(&self, gfx: &Gfx) {
        let screen = gfx.screen_size().unwrap();
        let board_w_px = self.cell_px * BOARD_W as isize;
        let board_h_px = self.cell_px * self.board_h as isize;
        let hud_x0 = screen.x - HUD_WIDTH;
        let tl_x = (self.origin_x - FRAME_PAD).max(0);
        let tl_y = (self.origin_y - FRAME_PAD).max(0);
        let br_x = (self.origin_x + board_w_px - 1 + FRAME_PAD).min(hud_x0 - 1);
        let br_y = (self.origin_y + board_h_px - 1 + FRAME_PAD).min(screen.y - 1);
        let frame = RoundedRectangle {
            border: Rectangle {
                tl: Point::new(tl_x, tl_y),
                br: Point::new(br_x, br_y),
                style: DrawStyle::new(PixelColor::Dark, PixelColor::Light, 1),
            },
            radius: 1,
        };
        gfx.draw_rounded_rectangle(frame).ok();
    }

    // next piece and score display
    fn draw_hud(&self, gfx: &Gfx) {
        let screen = gfx.screen_size().unwrap();
        let hud_x0 = screen.x - HUD_WIDTH;

        // helps prevent text clipping
        let board_right_edge = self.origin_x + self.cell_px * BOARD_W as isize;
        let text_x0 = board_right_edge.min(hud_x0);

        let mut label_tv = TextView::new(
            Gid::dummy(),
            TextBounds::CenteredTop(Rectangle::new(Point::new(text_x0, 0), Point::new(screen.x, 12))),
        );
        label_tv.style = GlyphStyle::Small;
        label_tv.draw_border = false;
        label_tv.clear_area = false;
        write!(label_tv, "Score").ok();
        gfx.draw_textview(&mut label_tv).ok();

        let mut score_tv = TextView::new(
            Gid::dummy(),
            TextBounds::CenteredTop(Rectangle::new(Point::new(text_x0, 12), Point::new(screen.x, 24))),
        );
        score_tv.style = GlyphStyle::Small;
        score_tv.draw_border = false;
        score_tv.clear_area = false;
        write!(score_tv, "{}", self.score).ok();
        gfx.draw_textview(&mut score_tv).ok();

        let mut next_label_tv = TextView::new(
            Gid::dummy(),
            TextBounds::CenteredTop(Rectangle::new(Point::new(text_x0, 40), Point::new(screen.x, 52))),
        );
        next_label_tv.style = GlyphStyle::Small;
        next_label_tv.draw_border = false;
        next_label_tv.clear_area = false;
        write!(next_label_tv, "Next").ok();
        gfx.draw_textview(&mut next_label_tv).ok();

        let preview_cell_px: isize = 5;
        let preview_origin_x = hud_x0 + (HUD_WIDTH - preview_cell_px * 4) / 2;
        let preview_origin_y: isize = 54;
        for (dx, dy) in Self::cells_for(self.next_piece, 0) {
            let x0 = preview_origin_x + dx as isize * preview_cell_px;
            let y0 = preview_origin_y + dy as isize * preview_cell_px;
            let rect = RoundedRectangle {
                border: Rectangle {
                    tl: Point::new(x0, y0),
                    br: Point::new(x0 + preview_cell_px - 1, y0 + preview_cell_px - 1),
                    style: DrawStyle::new(PixelColor::Dark, PixelColor::Light, 1),
                },
                radius: 1,
            };
            gfx.draw_rounded_rectangle(rect).ok();
        }
    }
}
