// Tetris — all seven standard pieces, 4-way rotation (simplified wall-kick, no full SRS
// kick table), 7-bag randomization, a score counter, a side-mounted HUD (score + next-piece
// preview), and a flash animation for row clears.
// Rendering uses gfx.draw_rounded_rectangle(), matching the pattern in ux.rs's totp_box, and
// text uses TextView/gfx.draw_textview(), matching the pattern in ux.rs's battery/attach labels.

use core::fmt::Write;
use std::collections::VecDeque;

use blitstr2::GlyphStyle;
use ux_api::minigfx::*;
use ux_api::service::api::Gid;
use ux_api::service::gfx::Gfx;

const BOARD_W: usize = 10;
// Board height is not fixed - it's picked at construction time from the real screen so we
// always get a standard-width (10 col) board, as tall as the panel reasonably allows,
// without ever exceeding it. See `new()`.
const MIN_BOARD_H: usize = 16;
const MAX_BOARD_H: usize = 24;

// Fixed column reserved on the right edge of the screen for the score readout and the
// next-piece preview, subtracted from the usable width before the board is sized. Reserving
// this up front (rather than trying to steal space from whatever margin the board sizing
// happens to leave) keeps the HUD available regardless of the panel's aspect ratio.
const HUD_WIDTH: isize = 34;

// Vertical breathing room reserved above and below the board for draw_board_frame()'s outline
// (see below). Reserved up front here, the same way HUD_WIDTH is reserved off the width,
// rather than drawn as an afterthought outset from whatever centering happened to leave over -
// the row-fitting math below greedily fills all available height, so without this the leftover
// slack is just a division remainder (often less than FRAME_PAD, sometimes exactly 0).
const FRAME_PAD: isize = 3;

// main.rs's Tetris timer thread now fires a raw tick every 10ms - far faster than the board
// should actually move - so gravity speed is tuned here instead, as a tick count rather than
// wall-clock time. GRAVITY_TICKS * 10ms = the delay between gravity steps; tune this to
// change gravity speed (60 ticks = 600ms, the original speed).
const GRAVITY_TICKS: u32 = 60;

// Clear-flash timing, also in raw ticks: each blink phase (on or off) lasts
// CLEAR_ANIM_TICKS_PER_FRAME ticks, and the flash runs for CLEAR_ANIM_FRAMES phases before the
// rows collapse. 8 * 10ms = 80ms/phase, 4 phases = 320ms total - two quick blinks.
const CLEAR_ANIM_TICKS_PER_FRAME: u8 = 8;
const CLEAR_ANIM_FRAMES: u8 = 4;

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

/// Tracks an in-progress row-clear flash: which board rows are involved, and how many ticks
/// have elapsed. While this is `Some`, gravity and player input are both paused - there's no
/// active falling piece during the flash, matching the brief freeze most Tetris games do on a
/// clear.
struct ClearAnim {
    rows: Vec<usize>,
    frame: u8,
    // Raw ticks elapsed within the current frame/phase - see CLEAR_ANIM_TICKS_PER_FRAME.
    tick: u8,
}

pub struct TetrisGame {
    // Vec instead of a fixed-size array because board_h now varies per-instance depending on
    // the panel's resolution.
    board: Vec<[bool; BOARD_W]>,
    board_h: usize,
    piece: Piece,
    // Rotation state 0..=3 (spawn, 90° CW, 180°, 270° CW), replacing the old two-state
    // `rotated: bool` now that every piece supports full 4-way rotation.
    rotation: u8,
    px: i32,
    py: i32,
    // 7-bag queue: pieces are drawn from the front, and a fresh shuffled set of all seven is
    // appended whenever fewer than 2 remain (current + next-preview always need to be
    // resolvable). This guarantees no piece repeats more than once every 7 spawns while still
    // being effectively random, which is much less streaky than pure per-spawn randomization.
    queue: VecDeque<Piece>,
    next_piece: Piece,
    score: u32,
    clearing: Option<ClearAnim>,
    // Raw ticks elapsed since the last gravity step - see GRAVITY_TICKS.
    gravity_accum: u32,
    game_over: bool,
    // Derived from the real panel size at construction time (see `new`) instead of hardcoded
    // placeholders, so the bottom row of the board is guaranteed to land on-screen no matter
    // what the actual resolution turns out to be.
    cell_px: isize,
    origin_x: isize,
    origin_y: isize,
}

impl TetrisGame {
    pub fn new(gfx: &Gfx) -> Self {
        let screen = gfx.screen_size().unwrap();
        // Width available to the board once the HUD column is set aside on the right.
        let usable_w = (screen.x - HUD_WIDTH).max(BOARD_W as isize);
        // Height available to the board once FRAME_PAD is set aside top and bottom for
        // draw_board_frame()'s outline - mirrors usable_w's reservation above.
        let usable_h = (screen.y - 2 * FRAME_PAD).max(MIN_BOARD_H as isize);

        // Width is always exactly BOARD_W (10) cells - size cells off the (HUD-reduced) width
        // first.
        let cell_px_by_width = (usable_w / BOARD_W as isize).max(1);
        let rows_that_fit = (usable_h / cell_px_by_width).max(0) as usize;

        let (cell_px, board_h) = if rows_that_fit >= MIN_BOARD_H {
            // Normal case: width-driven cell size gives us at least the minimum playable
            // height. Use as many rows as fit, capped at MAX_BOARD_H.
            (cell_px_by_width, rows_that_fit.min(MAX_BOARD_H))
        } else {
            // Screen is unusually short relative to its (HUD-reduced) width - width-driven
            // cells would leave less than MIN_BOARD_H rows visible. Fall back to sizing cells
            // off the height instead, guaranteeing the minimum board height. Also cap by the
            // width-driven size, so a tall/narrow panel can't size the board wider than the
            // room actually left after reserving the HUD column.
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
        // Normal cells are just an outline (dark border, light/empty fill) - filled is used
        // only for the clear-flash "on" frames, giving a solid highlight pop against the
        // otherwise outlined board.
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

    // Standard tetromino rotation states, each expressed as 4 (col, row) offsets within a
    // bounding box (2x2 for O, 4x4 for I, 3x3 for the rest). This is a v0 "simple" rotation -
    // rotating just re-checks the new orientation with a small set of horizontal nudges (see
    // `rotate`) rather than a full SRS wall-kick table.
    fn cells_for(piece: Piece, rotation: u8) -> [(i32, i32); 4] {
        match piece {
            // Rotation-invariant.
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

    /// Rotate 90° clockwise in place, nudging left/right (a minimal wall-kick) if the naive
    /// rotation would clip a wall. No-ops if nothing works (e.g. boxed in), for the O piece
    /// (rotation-invariant), or while a row-clear flash is playing.
    pub fn rotate(&mut self) {
        if self.game_over || self.clearing.is_some() || self.piece == Piece::O {
            return;
        }
        let new_rotation = (self.rotation + 1) % 4;
        let new_cells = Self::cells_for(self.piece, new_rotation);
        for kick in [0, -1, 1, -2, 2] {
            if !self.collides_shape(self.px + kick, self.py, new_cells) {
                self.px += kick;
                self.rotation = new_rotation;
                return;
            }
        }
        // no valid position found - leave orientation unchanged
    }

    /// Locks the current piece into the board. If that completes any rows, they aren't removed
    /// immediately - instead `clearing` is set so `draw()` can flash them and `gravity_tick()`
    /// can advance the flash a frame at a time; the actual collapse + next spawn happens in
    /// `finish_clear()` once the flash finishes. If no rows completed, the next piece spawns
    /// right away as before.
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

    // Simple, level-less line-clear scoring (single/double/triple/tetris), matching the
    // classic guideline base values.
    fn score_for(lines_cleared: u32) -> u32 {
        match lines_cleared {
            1 => 100,
            2 => 300,
            3 => 500,
            4 => 800,
            _ => 0,
        }
    }

    /// Collapses whatever rows were flagged by `lock()`, rebuilding the board bottom-up from
    /// everything that *wasn't* one of them (same "keep the rows that survive" approach the
    /// original single-pass clear used), then spawns the next piece.
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

    // Tops up the 7-bag queue whenever fewer than 2 pieces remain in it (current spawn needs
    // one, and the next-piece preview needs to be able to peek one more). Each top-up appends
    // one full shuffled set of all seven pieces, so within any run of 7 consecutive spawns every
    // piece appears exactly once - the standard "bag" randomizer, which avoids the long
    // same-piece droughts/streaks that pure per-spawn randomness can produce.
    fn ensure_queue(queue: &mut VecDeque<Piece>) {
        while queue.len() < 2 {
            let mut bag = ALL_PIECES;
            // Fisher-Yates shuffle. Uses RngCore::next_u32() directly (the same primitive
            // already relied on elsewhere in this codebase - see config.rs's nonce
            // generation via rand::thread_rng().fill_bytes()) rather than the higher-level
            // Rng::gen_range, so this doesn't depend on any rand API surface beyond what's
            // already proven to build here.
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
        // Each new piece starts its own fresh gravity countdown, rather than inheriting
        // whatever partial accumulation was left over from whatever just locked.
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

    /// Hard drop: slam the piece straight down to wherever it lands and lock it immediately.
    /// Bound to the down button instead of a single-step soft drop - sidesteps the upstream
    /// key-repeat delay entirely, since a single press now does the whole drop.
    pub fn hard_drop(&mut self) {
        if self.game_over || self.clearing.is_some() {
            return;
        }
        while !self.collides(self.px, self.py + 1) {
            self.py += 1;
        }
        self.lock();
    }

    /// Called on every raw tick from main.rs's fast (10ms) timer thread. Most calls are no-ops
    /// visually - this just counts ticks until it's actually time to either step gravity or
    /// advance the clear-flash by one phase - so it returns whether anything visible changed,
    /// letting the caller skip redrawing (and flushing the display) on the ticks that didn't.
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
                    // Flash: solid highlight on "on" frames, plain outline on "off" frames -
                    // the whole row blinks a couple of times before it collapses.
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

    // Outline around the whole play field. Each cell already draws its own border, but an
    // empty edge column/row has nothing but background on its outer side (only occupied/
    // clearing cells get drawn - see the loop below), so the actual limits of the board
    // weren't visible until something was stacked against them. This draws once, before the
    // per-cell loop, so the per-cell fills painted afterward sit cleanly on top of its
    // interior and only the traced perimeter (plus the padding ring around it) survives.
    fn draw_board_frame(&self, gfx: &Gfx) {
        let screen = gfx.screen_size().unwrap();
        let board_w_px = self.cell_px * BOARD_W as isize;
        let board_h_px = self.cell_px * self.board_h as isize;
        // A few pixels of breathing room between the cells and the outline, rather than
        // drawing it flush against them. FRAME_PAD is now reserved up front in `new()` (see
        // usable_h), so this should always have room; the clamps are just a defensive
        // fallback for edge-case panel sizes.
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

    // Score readout and next-piece preview, stacked in the HUD_WIDTH column reserved on the
    // right edge of the screen in `new()`.
    fn draw_hud(&self, gfx: &Gfx) {
        let screen = gfx.screen_size().unwrap();
        let hud_x0 = screen.x - HUD_WIDTH;

        // Text was clipped ("Score" losing its final "e") because its box ran flush from
        // hud_x0 to screen.x - the literal right edge of the physical display - so centered
        // text with zero margin to spare got its rightmost pixels cut by the screen edge
        // itself rather than by anything we control. `new()` centers the board inside
        // (screen.x - HUD_WIDTH), which almost always leaves a few leftover px between the
        // board's right edge and hud_x0 (from the integer-division centering). Anchoring text
        // to that real right edge instead of hud_x0 recovers that slack as extra width - it's
        // never negative (board_right_edge <= hud_x0 by construction) and never encroaches on
        // the board, so the reserved HUD_WIDTH column itself is untouched.
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

        // Small outlined squares in the same style as the board itself, just drawn at a
        // fixed, smaller cell size so the preview fits the HUD column regardless of how big
        // the board's own cells ended up being for this panel. Bumped up from 3 -> 5px/cell
        // (still centered within the same HUD_WIDTH column, so the column itself doesn't
        // change size) - HUD_WIDTH is 34px and a 4-cell-wide piece only needs 20px at this
        // size, so there's room to go up to ~8px/cell if you want it bigger still; the
        // limiting factor at that point is vertical space below preview_origin_y, not width.
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
