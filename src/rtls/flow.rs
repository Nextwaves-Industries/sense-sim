//! Where tags really go. A floor isn't used evenly: goods go from the doors to the racks and
//! back along a few aisles, and the corner behind the last rack may see nobody all week. From
//! the layout alone this works out those trips and how much of the traffic each spot carries,
//! so a plan is judged where tags spend their time. Routes you drew count for more than any
//! guess. It's a model of a layout, not a measurement of a site, and it says so wherever it
//! ends up.

use std::collections::BinaryHeap;

use bevy::math::Vec2;

use super::deploy::{FreeGrid, simplify};

/// What a place is to the people and vehicles moving between places.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaceKind {
    Door,
    Rack,
    Vehicle,
    /// The largest clear stretch of floor: where goods wait.
    Staging,
    Zone,
}

#[derive(Clone, Copy, Debug)]
pub struct Place {
    pub at: Vec2,
    pub kind: PlaceKind,
    /// How many trips it draws, relative to the others.
    pub pull: f32,
}

/// How much trips between two kinds of place count. Goods mostly cross between doors, staging
/// and racks. These are assumptions about how a warehouse works, with no measurement of any
/// one site behind them.
fn trip_weight(a: PlaceKind, b: PlaceKind) -> f32 {
    use PlaceKind::*;
    match (a, b) {
        (Door, Rack) | (Rack, Door) | (Staging, Rack) | (Rack, Staging) => 1.0,
        (Door, Staging) | (Staging, Door) => 1.5,
        (Vehicle, _) | (_, Vehicle) | (Zone, _) | (_, Zone) => 0.6,
        (Rack, Rack) => 0.15,
        (Door, Door) | (Staging, Staging) => 0.1,
    }
}

#[derive(Clone, Debug, Default)]
pub struct Flow {
    pub lo: Vec2,
    pub cell: f32,
    pub nx: usize,
    pub nz: usize,
    /// Traffic through each cell, averaging 1 over the walkable ones; 0 where nobody can walk.
    pub traffic: Vec<f32>,
    /// Trips the map came from, the busiest first.
    pub routes: Vec<Vec<Vec2>>,
    pub places: Vec<Place>,
    /// Cells anyone could walk, whether or not anyone does.
    pub walkable: usize,
}

impl Flow {
    /// Traffic at a spot, from the nearest cell. 1 (an average spot) where there's no map.
    pub fn at(&self, p: Vec2) -> f32 {
        if self.traffic.is_empty() {
            return 1.0;
        }
        let ix = (((p.x - self.lo.x) / self.cell) as i64).clamp(0, self.nx as i64 - 1) as usize;
        let iz = (((p.y - self.lo.y) / self.cell) as i64).clamp(0, self.nz as i64 - 1) as usize;
        self.traffic[iz * self.nx + ix]
    }

    /// How much of the traffic runs through the busiest `share` of the walkable floor, the
    /// plain-language measure of how uneven the site is.
    pub fn concentration(&self, share: f32) -> f32 {
        let mut v: Vec<f32> = self.traffic.iter().copied().filter(|t| *t > 0.0).collect();
        if v.is_empty() {
            return share;
        }
        v.sort_by(|a, b| b.total_cmp(a));
        // A share of all the floor anyone could walk, not just of the floor somebody does.
        let top = ((self.walkable.max(v.len()) as f32 * share).ceil() as usize).clamp(1, v.len());
        v[..top].iter().sum::<f32>() / v.iter().sum::<f32>().max(1e-6)
    }

    /// Closed walking tours for `n` tags over the busy routes, each from a different start.
    /// Doors and staging are where tours begin. Same layout, same tours.
    pub fn tours(&self, grid: &FreeGrid, n: usize) -> Vec<Vec<Vec2>> {
        let hubs: Vec<usize> = self.places.iter().enumerate().filter(|(_, p)| matches!(p.kind, PlaceKind::Door | PlaceKind::Staging)).map(|(i, _)| i).collect();
        let others: Vec<usize> = self.places.iter().enumerate().filter(|(_, p)| !matches!(p.kind, PlaceKind::Door | PlaceKind::Staging)).map(|(i, _)| i).collect();
        if self.places.len() < 2 {
            return Vec::new();
        }
        let mut out = Vec::new();
        for w in 0..n {
            // A hub, then several stops spread over the list, back to the hub between them.
            let hub = if hubs.is_empty() { w % self.places.len() } else { hubs[w % hubs.len()] };
            let mut stops = vec![hub];
            let count = others.len().clamp(1, 6);
            for k in 0..count {
                if others.is_empty() {
                    break;
                }
                let stop = others[(w * others.len() / n.max(1) + k * others.len() / count) % others.len()];
                stops.push(stop);
                if k % 2 == 1 {
                    stops.push(hub);
                }
            }
            if stops.last() != Some(&hub) {
                stops.push(hub);
            }
            let cells: Vec<(usize, usize)> = stops.iter().filter_map(|s| grid.nearest_free(self.places[*s].at)).collect();
            let mut pts: Vec<Vec2> = Vec::new();
            for leg in cells.windows(2) {
                if let Some(seg) = grid.path(leg[0], leg[1]) {
                    pts.extend(seg);
                }
            }
            let pts = simplify(&pts);
            if pts.len() >= 2 {
                out.push(pts);
            }
        }
        out
    }
}

/// Per free cell, how many cells away the nearest wall or object is. Walking down the middle
/// of an aisle is cheaper than scraping along a rack.
fn clearance(g: &FreeGrid) -> Vec<f32> {
    let n = g.nx * g.nz;
    let mut d = vec![f32::MAX; n];
    let mut queue = std::collections::VecDeque::new();
    for iz in 0..g.nz {
        for ix in 0..g.nx {
            let k = iz * g.nx + ix;
            if !g.free[k] {
                d[k] = 0.0;
                queue.push_back(k);
            } else if ix == 0 || iz == 0 || ix + 1 == g.nx || iz + 1 == g.nz {
                d[k] = 1.0;
                queue.push_back(k);
            }
        }
    }
    while let Some(k) = queue.pop_front() {
        let (ix, iz) = (k % g.nx, k / g.nx);
        let next = d[k] + 1.0;
        let mut try_cell = |j: usize, queue: &mut std::collections::VecDeque<usize>| {
            if next < d[j] {
                d[j] = next;
                queue.push_back(j);
            }
        };
        if ix > 0 {
            try_cell(k - 1, &mut queue);
        }
        if ix + 1 < g.nx {
            try_cell(k + 1, &mut queue);
        }
        if iz > 0 {
            try_cell(k - g.nx, &mut queue);
        }
        if iz + 1 < g.nz {
            try_cell(k + g.nx, &mut queue);
        }
    }
    d
}

/// The free cell furthest from anything: the natural place for goods to wait.
pub fn clearest_spot(g: &FreeGrid) -> Option<Vec2> {
    let d = clearance(g);
    let mut best: Option<(f32, usize)> = None;
    for (k, c) in d.iter().enumerate() {
        if g.free[k] && best.is_none_or(|(b, _)| *c > b) {
            best = Some((*c, k));
        }
    }
    best.map(|(_, k)| g.center(k % g.nx, k / g.nx))
}

#[derive(PartialEq)]
struct Item(f32, usize);
impl Eq for Item {}
impl Ord for Item {
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        o.0.total_cmp(&self.0).then(o.1.cmp(&self.1))
    }
}
impl PartialOrd for Item {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}

/// Cheapest ways out from `from` over the free cells: for each cell, the one before it.
fn ways_from(g: &FreeGrid, clear: &[f32], from: usize) -> Vec<u32> {
    const NONE: u32 = u32::MAX;
    let n = g.nx * g.nz;
    let mut cost = vec![f32::MAX; n];
    let mut before = vec![NONE; n];
    let mut open = BinaryHeap::new();
    cost[from] = 0.0;
    open.push(Item(0.0, from));
    // Crossing a cell costs more the closer it is to something, up to double.
    let step = |k: usize| 1.0 + 1.0 / clear[k].max(1.0);
    while let Some(Item(c, k)) = open.pop() {
        if c > cost[k] {
            continue;
        }
        let (ix, iz) = ((k % g.nx) as i64, (k / g.nx) as i64);
        for (dx, dz) in [(1i64, 0i64), (-1, 0), (0, 1), (0, -1), (1, 1), (1, -1), (-1, 1), (-1, -1)] {
            let (x, z) = (ix + dx, iz + dz);
            if x < 0 || z < 0 || x >= g.nx as i64 || z >= g.nz as i64 {
                continue;
            }
            let j = z as usize * g.nx + x as usize;
            if !g.free[j] {
                continue;
            }
            // No squeezing diagonally past a corner.
            if dx != 0 && dz != 0 && (!g.free[iz as usize * g.nx + x as usize] || !g.free[z as usize * g.nx + ix as usize]) {
                continue;
            }
            let length = if dx != 0 && dz != 0 { std::f32::consts::SQRT_2 } else { 1.0 };
            let next = c + length * step(j);
            if next < cost[j] {
                cost[j] = next;
                before[j] = k as u32;
                open.push(Item(next, j));
            }
        }
    }
    before
}

/// Spreads traffic over the free cells: people don't all walk the one centre line.
fn spread(g: &FreeGrid, cells: &[f32], radius_m: f32) -> Vec<f32> {
    let reach = ((radius_m / g.cell).ceil() as i64).max(1);
    let sigma = (radius_m / g.cell).max(0.5);
    let kernel: Vec<f32> = (-reach..=reach).map(|d| (-0.5 * (d as f32 / sigma).powi(2)).exp()).collect();
    let pass = |src: &[f32], along_x: bool| -> Vec<f32> {
        let mut out = vec![0.0; src.len()];
        for iz in 0..g.nz as i64 {
            for ix in 0..g.nx as i64 {
                let k = (iz as usize) * g.nx + ix as usize;
                if !g.free[k] {
                    continue;
                }
                let (mut sum, mut total) = (0.0, 0.0);
                for (i, w) in kernel.iter().enumerate() {
                    let d = i as i64 - reach;
                    let (x, z) = if along_x { (ix + d, iz) } else { (ix, iz + d) };
                    if x < 0 || z < 0 || x >= g.nx as i64 || z >= g.nz as i64 {
                        continue;
                    }
                    let j = z as usize * g.nx + x as usize;
                    if g.free[j] {
                        sum += src[j] * w;
                        total += w;
                    }
                }
                out[k] = if total > 0.0 { sum / total } else { 0.0 };
            }
        }
        out
    };
    pass(&pass(cells, true), false)
}

/// Works out the traffic over a walkable grid from the places in it. `drawn` are routes somebody
/// actually set up, each with how many trips it counts for. With nothing to go on, an even map.
pub fn build(g: &FreeGrid, places: &[Place], drawn: &[(Vec<Vec2>, f32)]) -> Flow {
    let n = g.nx * g.nz;
    let free_cells = g.free.iter().filter(|f| **f).count();
    let mut flow = Flow { lo: g.lo, cell: g.cell, nx: g.nx, nz: g.nz, traffic: vec![0.0; n], routes: Vec::new(), places: places.to_vec(), walkable: free_cells };
    if free_cells == 0 {
        return flow;
    }
    let clear = clearance(g);
    // Where each place meets the floor. Places that can't be reached take no part.
    let spots: Vec<Option<usize>> = places.iter().map(|p| g.nearest_free(p.at).map(|(x, z)| z * g.nx + x)).collect();
    let mut trips: Vec<(f32, Vec<usize>)> = Vec::new();
    for (i, a) in places.iter().enumerate() {
        let Some(from) = spots[i] else { continue };
        let before = ways_from(g, &clear, from);
        for (j, b) in places.iter().enumerate().skip(i + 1) {
            let Some(to) = spots[j] else { continue };
            if from == to || (before[to] == u32::MAX) {
                continue;
            }
            let weight = a.pull * b.pull * trip_weight(a.kind, b.kind);
            if weight <= 0.0 {
                continue;
            }
            let mut path = vec![to];
            let mut k = to;
            while k != from {
                k = before[k] as usize;
                path.push(k);
            }
            for c in &path {
                flow.traffic[*c] += weight;
            }
            trips.push((weight, path));
        }
    }
    // Drawn routes outweigh the guess: as much as all guessed trips together, shared out.
    let guessed: f32 = trips.iter().map(|t| t.0).sum();
    let drawn_total: f32 = drawn.iter().map(|d| d.1).sum();
    for (route, count) in drawn {
        let weight = if drawn_total > 0.0 { count / drawn_total * guessed.max(1.0) } else { 0.0 };
        let mut path = Vec::new();
        for leg in route.windows(2) {
            let steps = (leg[0].distance(leg[1]) / g.cell).ceil().max(1.0) as usize;
            for s in 0..=steps {
                let p = leg[0].lerp(leg[1], s as f32 / steps as f32);
                let ix = (((p.x - g.lo.x) / g.cell) as i64).clamp(0, g.nx as i64 - 1) as usize;
                let iz = (((p.y - g.lo.y) / g.cell) as i64).clamp(0, g.nz as i64 - 1) as usize;
                let k = iz * g.nx + ix;
                if g.free[k] && path.last() != Some(&k) {
                    path.push(k);
                }
            }
        }
        for c in &path {
            flow.traffic[*c] += weight;
        }
        if !path.is_empty() {
            trips.push((weight, path));
        }
    }
    if trips.is_empty() {
        // Nothing to go on: every walkable cell the same.
        for (t, f) in flow.traffic.iter_mut().zip(&g.free) {
            *t = if *f { 1.0 } else { 0.0 };
        }
        return flow;
    }
    trips.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
    flow.routes = trips.iter().take(12).map(|(_, path)| simplify(&path.iter().map(|k| g.center(k % g.nx, k / g.nx)).collect::<Vec<_>>())).collect();
    flow.traffic = spread(g, &flow.traffic, 0.75);
    let mean = flow.traffic.iter().sum::<f32>() / free_cells as f32;
    if mean > 0.0 {
        flow.traffic.iter_mut().for_each(|t| *t /= mean);
    }
    flow
}

/// A point's weight from its traffic: never nothing, because a tag can end up anywhere, but a
/// busy aisle counts for many times a quiet corner. `traffic` averages 1.
pub fn demand(traffic: f32) -> f32 {
    const EVERYWHERE: f32 = 0.2;
    EVERYWHERE + (1.0 - EVERYWHERE) * traffic
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::math::Vec3;

    /// A 20 × 12 m hall with two long racks, leaving aisles along z = 3 and z = 9 and a cross
    /// aisle at each end.
    fn hall() -> FreeGrid {
        let boxes = vec![(Vec3::new(3.0, 0.0, 5.0), Vec3::new(17.0, 3.0, 7.0))];
        FreeGrid::new(Vec2::ZERO, Vec2::new(20.0, 12.0), 0.5, &boxes, 0.3)
    }

    fn place(x: f32, z: f32, kind: PlaceKind) -> Place {
        Place { at: Vec2::new(x, z), kind, pull: 1.0 }
    }

    #[test]
    fn trips_stay_on_the_floor_and_load_the_way_between_places() {
        let g = hall();
        // A dock at one end of the near aisle, a rack face at the other.
        let f = build(&g, &[place(1.0, 3.0, PlaceKind::Door), place(15.0, 3.5, PlaceKind::Rack)], &[]);
        for (k, t) in f.traffic.iter().enumerate() {
            assert!(g.free[k] || *t == 0.0, "traffic inside the rack at {k}");
        }
        // Down the middle of the aisle, not scraping along the rack.
        let (busy, by_the_rack, quiet) = (f.at(Vec2::new(8.0, 2.5)), f.at(Vec2::new(8.0, 4.4)), f.at(Vec2::new(8.0, 10.5)));
        assert!(busy > 3.0, "{busy}");
        assert!(by_the_rack < busy, "{by_the_rack} against {busy}");
        assert_eq!(quiet, 0.0);
        let mean: f32 = f.traffic.iter().sum::<f32>() / g.free.iter().filter(|x| **x).count() as f32;
        assert!((mean - 1.0).abs() < 1e-3, "{mean}");
    }

    #[test]
    fn nothing_to_go_on_is_an_even_floor() {
        let g = hall();
        let f = build(&g, &[], &[]);
        assert_eq!(f.at(Vec2::new(8.0, 3.5)), 1.0);
        assert_eq!(f.at(Vec2::new(8.0, 10.5)), 1.0);
        assert!((f.concentration(0.2) - 0.2).abs() < 0.02, "{}", f.concentration(0.2));
    }

    #[test]
    fn a_drawn_route_outweighs_the_guess() {
        let g = hall();
        let places = [place(1.0, 3.0, PlaceKind::Door), place(15.0, 3.5, PlaceKind::Rack)];
        let guess = build(&g, &places, &[]);
        assert_eq!(guess.at(Vec2::new(8.0, 10.5)), 0.0);
        let drawn = vec![(vec![Vec2::new(2.0, 10.5), Vec2::new(18.0, 10.5)], 10.0)];
        let both = build(&g, &places, &drawn);
        assert!(both.at(Vec2::new(8.0, 10.5)) > 1.0, "{}", both.at(Vec2::new(8.0, 10.5)));
    }

    #[test]
    fn real_sites_are_uneven_and_the_measure_says_so() {
        let g = hall();
        let f = build(&g, &[place(1.0, 3.0, PlaceKind::Door), place(15.0, 3.5, PlaceKind::Rack), place(15.0, 8.5, PlaceKind::Rack)], &[]);
        // Even would be 0.2. The real sample sites come out at 0.4 to 0.5.
        assert!(f.concentration(0.2) > 0.3, "{}", f.concentration(0.2));
    }

    #[test]
    fn tours_are_closed_walkable_and_the_same_every_time() {
        let g = hall();
        let places = [place(1.0, 3.0, PlaceKind::Door), place(15.0, 3.5, PlaceKind::Rack), place(15.0, 8.5, PlaceKind::Rack), place(5.0, 9.0, PlaceKind::Rack)];
        let f = build(&g, &places, &[]);
        let tours = f.tours(&g, 2);
        assert_eq!(tours.len(), 2);
        assert_ne!(tours[0], tours[1], "each from its own start");
        for t in &tours {
            assert!(t.len() >= 3);
            assert!(t[0].distance(*t.last().unwrap()) < 0.8, "closed: {t:?}");
        }
        assert_eq!(tours, f.tours(&g, 2));
    }

    #[test]
    fn staging_is_the_clearest_floor() {
        // Racks fill the far end, leaving 3 m aisles beside them and a big clear end.
        let boxes = vec![(Vec3::new(8.0, 0.0, 3.0), Vec3::new(20.0, 3.0, 9.0))];
        let g = FreeGrid::new(Vec2::ZERO, Vec2::new(20.0, 12.0), 0.5, &boxes, 0.3);
        let s = clearest_spot(&g).unwrap();
        assert!(s.x < 8.0, "the clear end, not an aisle beside the racks: {s}");
    }

    #[test]
    fn a_map_never_writes_a_spot_off() {
        assert!(demand(0.0) > 0.0);
        assert!(demand(5.0) > 4.0 * demand(0.0));
        assert!((demand(1.0) - 1.0).abs() < 1e-6, "an average spot weighs what it always did");
    }
}
