//! Pickups, weapons, traps and shields.
//!
//! All of it runs inside the fixed-timestep sim and is driven by a seeded
//! generator, so a race is reproducible and every behaviour here can be raced
//! and measured headless, on a machine with no GPU.
//!
//! The design rule throughout is the one the handling already follows: a hit
//! costs you places, never the car. Being shot takes your engine for a moment
//! and spins you; it does not end your race, any more than being upside down
//! does.

use glam::Vec3;

use crate::track::Track;
use crate::vehicle::Vehicle;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Weapon {
    /// Fired forward, leans toward whatever is ahead of it.
    Rocket,
    /// Dropped behind, arms, then waits.
    Mine,
    /// Absorbs the next hit.
    Shield,
    /// Reaches the car directly ahead in the running order, wherever it is.
    Shock,
}

impl Weapon {
    pub fn name(self) -> &'static str {
        match self {
            Weapon::Rocket => "ROCKET",
            Weapon::Mine => "MINE",
            Weapon::Shield => "SHIELD",
            Weapon::Shock => "SHOCK",
        }
    }
}

/// A crate on the track surface. Collected by driving through it, then gone for
/// a while so one pad cannot arm a whole field.
pub struct Pickup {
    pub pos: Vec3,
    pub up: Vec3,
    pub frame: usize,
    /// Seconds until it returns. Zero means it is there now.
    pub cooldown: f32,
}

pub struct Rocket {
    pub pos: Vec3,
    pub vel: Vec3,
    pub owner: usize,
    pub life: f32,
    hint: usize,
}

pub struct Mine {
    pub pos: Vec3,
    pub up: Vec3,
    /// Counts down to zero before the mine can be set off. An armed mine
    /// catches anyone; this delay is the only thing protecting the driver who
    /// dropped it, and at racing speed it puts them well clear first.
    pub arm: f32,
    pub life: f32,
}

/// Something that happened this tick, for the sound and the particles to read.
/// Kept as a list rather than as callbacks so the sim stays free of rendering.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Event {
    Collected { driver: usize, weapon: Weapon },
    Fired { driver: usize, weapon: Weapon, pos: Vec3 },
    /// A hit that landed.
    Struck { driver: usize, pos: Vec3 },
    /// A hit a shield ate.
    Blocked { driver: usize, pos: Vec3 },
}

const PICKUP_SPACING_M: f32 = 150.0;
const PICKUP_RADIUS: f32 = 5.0;
const PICKUP_RESPAWN: f32 = 10.0;

const ROCKET_SPEED: f32 = 280.0;
const ROCKET_LIFE: f32 = 7.0;
/// How hard a rocket leans toward its target, in metres per second squared.
/// Enough that a rocket which gets a lock usually connects: dodging one is
/// meant to mean breaking the seeker cone or spending a shield, not out-running
/// its steering.
const ROCKET_HOMING: f32 = 420.0;
/// Rockets only chase what is roughly in front of them; a rocket that turns
/// round and comes back is a bug, not a feature.
const ROCKET_CONE: f32 = 0.55;
/// Blast radius, not a contact radius. A car is 4.2 m long, so anything much
/// tighter than this counts a rocket passing a couple of metres off the
/// bodywork as a clean miss.
const ROCKET_HIT_RADIUS: f32 = 6.0;
/// How far off the tube wall a rocket tries to stay.
const ROCKET_STANDOFF: f32 = 2.5;

const MINE_ARM_TIME: f32 = 1.0;
const MINE_LIFE: f32 = 30.0;
const MINE_HIT_RADIUS: f32 = 5.0;

const SHIELD_TIME: f32 = 7.0;
const STUN_TIME: f32 = 1.3;
/// Impulse a hit delivers, in newton-seconds.
const HIT_IMPULSE: f32 = 26_000.0;
const HIT_SPIN: f32 = 4.5;

/// Longest a shock can reach. Generous, because its whole point is to reach the
/// leader from a long way back, but not unlimited.
const SHOCK_RANGE: f32 = 900.0;
/// How long a driver waits between shots, so holding the button does not empty
/// the arsenal in three frames.
const FIRE_COOLDOWN: f32 = 0.4;

pub struct Arsenal {
    pub pickups: Vec<Pickup>,
    pub rockets: Vec<Rocket>,
    pub mines: Vec<Mine>,
    /// What each driver is holding. Index 0 is the player.
    pub held: Vec<Option<Weapon>>,
    cooldowns: Vec<f32>,
    /// AI patience, so opponents do not all fire on the same tick.
    ai_delay: Vec<f32>,
    rng: u64,
    pub events: Vec<Event>,
    /// Weapons off makes this a pure racing game, crates and all.
    pub enabled: bool,
    /// How readily the AI shoots, from the difficulty. 1 is the baseline.
    pub aggression: f32,
}

impl Arsenal {
    pub fn new(track: &Track, seed: u64, drivers: usize) -> Arsenal {
        Arsenal {
            pickups: place_pickups(track),
            rockets: Vec::new(),
            mines: Vec::new(),
            held: vec![None; drivers],
            cooldowns: vec![0.0; drivers],
            ai_delay: vec![0.0; drivers],
            rng: seed | 1,
            events: Vec::new(),
            enabled: true,
            aggression: 1.0,
        }
    }

    fn next(&mut self) -> f32 {
        // xorshift64*, matching the track generator rather than pulling in a
        // crate, and seeded per race so a replay is a replay.
        self.rng ^= self.rng >> 12;
        self.rng ^= self.rng << 25;
        self.rng ^= self.rng >> 27;
        let v = self.rng.wrapping_mul(0x2545_F491_4F6C_DD1D);
        ((v >> 40) as f32) / (1u32 << 24) as f32
    }

    /// What a driver gets from a crate, weighted by how they are doing.
    ///
    /// The leader draws defence and traps; the field draws things that reach
    /// forward. That is what keeps a race close without ever taking a place
    /// away from someone who earned it.
    fn roll(&mut self, rank: usize, drivers: usize) -> Weapon {
        let behind = if drivers > 1 {
            rank as f32 / (drivers - 1) as f32
        } else {
            0.0
        };
        let r = self.next();
        // Shares that slide with position: rockets and shocks grow toward the
        // back, shields and mines toward the front.
        let rocket = 0.18 + 0.30 * behind;
        let shock = rocket + 0.04 + 0.26 * behind;
        let mine = shock + 0.30 - 0.14 * behind;
        if r < rocket {
            Weapon::Rocket
        } else if r < shock {
            Weapon::Shock
        } else if r < mine {
            Weapon::Mine
        } else {
            Weapon::Shield
        }
    }

    /// `progress` is each driver's unwrapped distance, used for the running
    /// order that both the pickup weighting and the shock depend on.
    pub fn update(
        &mut self,
        track: &Track,
        cars: &mut [&mut Vehicle],
        fire: &[bool],
        progress: &[f32],
        dt: f32,
    ) {
        self.events.clear();
        if !self.enabled {
            return;
        }
        let drivers = cars.len();
        // Rank 0 is the leader.
        let mut order: Vec<usize> = (0..drivers).collect();
        order.sort_by(|a, b| progress[*b].total_cmp(&progress[*a]));
        let mut rank = vec![0usize; drivers];
        for (place, driver) in order.iter().enumerate() {
            rank[*driver] = place;
        }

        for c in self.cooldowns.iter_mut() {
            *c = (*c - dt).max(0.0);
        }

        self.collect(cars, &rank, drivers, dt);
        self.fire_weapons(track, cars, fire, &order, &rank, dt);
        self.move_rockets(track, cars, dt);
        self.tick_mines(cars, dt);
    }

    fn collect(&mut self, cars: &mut [&mut Vehicle], rank: &[usize], drivers: usize, dt: f32) {
        for index in 0..self.pickups.len() {
            if self.pickups[index].cooldown > 0.0 {
                self.pickups[index].cooldown -= dt;
                continue;
            }
            let pos = self.pickups[index].pos;
            let Some(driver) = cars
                .iter()
                .position(|c| c.pos.distance_squared(pos) < PICKUP_RADIUS * PICKUP_RADIUS)
            else {
                continue;
            };
            // Already holding something: the crate stays for someone else.
            if self.held[driver].is_some() {
                continue;
            }
            let weapon = self.roll(rank[driver], drivers);
            self.held[driver] = Some(weapon);
            self.pickups[index].cooldown = PICKUP_RESPAWN;
            self.events.push(Event::Collected { driver, weapon });
        }
    }

    fn fire_weapons(
        &mut self,
        track: &Track,
        cars: &mut [&mut Vehicle],
        fire: &[bool],
        order: &[usize],
        rank: &[usize],
        dt: f32,
    ) {
        for driver in 0..cars.len() {
            let Some(weapon) = self.held[driver] else {
                continue;
            };
            if self.cooldowns[driver] > 0.0 {
                continue;
            }
            // The player says when. Everyone else is judged.
            let wants = if driver == 0 {
                fire.first().copied().unwrap_or(false)
            } else {
                self.ai_wants_to_fire(driver, weapon, cars, rank, dt)
            };
            if !wants {
                continue;
            }

            let car = &cars[driver];
            let pos = car.pos;
            let forward = car.forward();
            let up = car.up();
            let vel = car.vel;
            self.held[driver] = None;
            self.cooldowns[driver] = FIRE_COOLDOWN;
            self.events.push(Event::Fired { driver, weapon, pos });

            match weapon {
                Weapon::Rocket => self.rockets.push(Rocket {
                    // Clear of the nose, or it collides with the car firing it.
                    pos: pos + forward * 3.5 + up * 0.4,
                    vel: vel + forward * ROCKET_SPEED,
                    owner: driver,
                    life: ROCKET_LIFE,
                    hint: car.hint,
                }),
                Weapon::Mine => {
                    let surface = track.surface(pos - forward * 5.0, car.hint);
                    let at = pos - forward * 5.0;
                    self.mines.push(Mine {
                        // Settled onto the road rather than left at hub height.
                        pos: at + surface.down * (surface.gap - 0.5),
                        up: -surface.down,
                        arm: MINE_ARM_TIME,
                        life: MINE_LIFE,
                    });
                }
                Weapon::Shield => cars[driver].shield = SHIELD_TIME,
                Weapon::Shock => {
                    // The car one place ahead, wherever it happens to be.
                    let place = rank[driver];
                    if place == 0 {
                        continue;
                    }
                    let target = order[place - 1];
                    let target_pos = cars[target].pos;
                    if target_pos.distance(pos) > SHOCK_RANGE {
                        continue;
                    }
                    // Struck from below, so a shock lifts and spins rather than
                    // shoving the victim off the racing line entirely.
                    let from = target_pos - cars[target].up() * 3.0;
                    self.strike(cars, target, from, 0.8);
                }
            }
        }
    }

    fn ai_wants_to_fire(
        &mut self,
        driver: usize,
        weapon: Weapon,
        cars: &[&mut Vehicle],
        rank: &[usize],
        dt: f32,
    ) -> bool {
        self.ai_delay[driver] -= dt;
        if self.ai_delay[driver] > 0.0 {
            return false;
        }
        // Re-check a few times a second rather than every tick, and stagger the
        // field so they do not all decide together.
        // Harder settings decide faster and so shoot sooner.
        let patience = (0.25 + self.next() * 0.35) / self.aggression.max(0.05);
        self.ai_delay[driver] = patience;

        let me = &cars[driver];
        match weapon {
            // Fire when something is genuinely in front and in range. The same
            // test the rocket itself uses, so the AI does not take shots its
            // rocket will not follow.
            Weapon::Rocket => cars.iter().enumerate().any(|(other, car)| {
                if other == driver {
                    return false;
                }
                let to = car.pos - me.pos;
                let range = to.length();
                range > 6.0 && range < 260.0 && to.normalize_or_zero().dot(me.forward()) > 0.80
            }),
            // Drop one when somebody is close behind and would run into it.
            Weapon::Mine => cars.iter().enumerate().any(|(other, car)| {
                if other == driver {
                    return false;
                }
                let to = car.pos - me.pos;
                let range = to.length();
                range < 70.0 && to.normalize_or_zero().dot(me.forward()) < -0.5
            }),
            // Nothing to gain by holding it, and holding it blocks a crate.
            Weapon::Shield => me.shield <= 0.0,
            Weapon::Shock => rank[driver] > 0,
        }
    }

    fn move_rockets(&mut self, track: &Track, cars: &mut [&mut Vehicle], dt: f32) {
        let mut hits: Vec<(usize, Vec3)> = Vec::new();
        let mut spent: Vec<usize> = Vec::new();

        for index in 0..self.rockets.len() {
            let rocket = &mut self.rockets[index];
            rocket.life -= dt;

            // Lean toward the best target in front of it.
            let mut best: Option<(f32, Vec3, Vec3)> = None;
            for (driver, car) in cars.iter().enumerate() {
                if driver == rocket.owner {
                    continue;
                }
                let to = car.pos - rocket.pos;
                let range = to.length();
                if range < 0.01 {
                    continue;
                }
                if to.dot(rocket.vel.normalize_or_zero()) / range < ROCKET_CONE {
                    continue;
                }
                if best.map_or(true, |(b, _, _)| range < b) {
                    best = Some((range, car.pos, car.vel));
                }
            }
            if let Some((range, target, target_vel)) = best {
                // Aim where the car will be, not where it is. Steering at the
                // present position is a tail chase: measured against a car
                // nine metres off the line, it closed ninety metres to nine
                // and then flew past, every time.
                let closing = (rocket.vel - target_vel).dot((target - rocket.pos) / range);
                let lead = (range / closing.max(20.0)).min(1.5);
                let want = (target + target_vel * lead - rocket.pos).normalize_or_zero();
                // Steering, not thrust. Only the part of the desired direction
                // that lies across the rocket's path actually turns it;
                // accelerating straight at a target that is nearly ahead puts
                // almost everything into speed, which the clamp below then
                // takes straight back out. Measured against a car fourteen
                // metres off the nose, pushing at the target closed four of
                // them and turning across the path closed all fourteen.
                let heading = rocket.vel.normalize_or_zero();
                let lateral = (want - heading * want.dot(heading)).normalize_or_zero();
                // Turn harder the closer it gets: the last few metres are where
                // a chase is won or lost.
                let urgency = 1.0 + 2.5 * (1.0 - (range / 40.0).clamp(0.0, 1.0));
                rocket.vel += lateral * (ROCKET_HOMING * urgency * dt);
                // Turning must not also accelerate it, or a chasing rocket
                // outruns everything on the track.
                let speed = rocket.vel.length();
                if speed > ROCKET_SPEED {
                    rocket.vel *= ROCKET_SPEED / speed;
                }
            }
            rocket.pos += rocket.vel * dt;

            // Keep it inside the tube. A rocket that leaves through the wall
            // and flies off into the scenery is just a missing rocket.
            let surface = track.surface(rocket.pos, rocket.hint);
            rocket.hint = surface.index;
            // Hold a standoff from the wall rather than bouncing off it. A
            // rocket flies straight while the tube bends, so it meets the
            // surface constantly; reflecting it there threw away the guidance
            // and left it skimming the floor past its target. Pushing it back
            // toward the tube interior, harder the closer it gets, lets it
            // follow the track without ever ricocheting.
            if surface.gap < ROCKET_STANDOFF {
                let interior = -surface.down;
                rocket.vel += interior * ((ROCKET_STANDOFF - surface.gap) * 60.0 * dt);
                // Backstop, for a rocket fired straight at a wall.
                if surface.gap < 0.3 {
                    rocket.pos += interior * (0.3 - surface.gap);
                }
            }

            let mut done = rocket.life <= 0.0;
            for (driver, car) in cars.iter().enumerate() {
                if driver == rocket.owner {
                    continue;
                }
                if car.pos.distance_squared(rocket.pos) < ROCKET_HIT_RADIUS * ROCKET_HIT_RADIUS {
                    hits.push((driver, rocket.pos));
                    done = true;
                    break;
                }
            }
            if done {
                spent.push(index);
            }
        }

        // Back to front, so removing one does not shift the next index.
        for index in spent.into_iter().rev() {
            self.rockets.swap_remove(index);
        }
        for (driver, at) in hits {
            self.strike(cars, driver, at, 1.0);
        }
    }

    fn tick_mines(&mut self, cars: &mut [&mut Vehicle], dt: f32) {
        let mut hits: Vec<(usize, Vec3)> = Vec::new();
        let mut spent: Vec<usize> = Vec::new();

        for index in 0..self.mines.len() {
            let mine = &mut self.mines[index];
            mine.arm = (mine.arm - dt).max(0.0);
            mine.life -= dt;
            if mine.life <= 0.0 {
                spent.push(index);
                continue;
            }
            if mine.arm > 0.0 {
                continue;
            }
            let at = mine.pos;
            // Once armed a mine catches anyone, its owner included: one you
            // could safely reverse over is not a trap. The arming delay is
            // what protects the driver who dropped it, and at any racing speed
            // it puts them a long way clear before the mine wakes up.
            if let Some(driver) = cars
                .iter()
                .position(|car| car.pos.distance_squared(at) < MINE_HIT_RADIUS * MINE_HIT_RADIUS)
            {
                hits.push((driver, at));
                spent.push(index);
            }
        }

        for index in spent.into_iter().rev() {
            self.mines.swap_remove(index);
        }
        for (driver, at) in hits {
            self.strike(cars, driver, at, 1.0);
        }
    }

    /// Land a hit on one car, unless its shield eats it.
    fn strike(&mut self, cars: &mut [&mut Vehicle], driver: usize, from: Vec3, scale: f32) {
        let spin = Vec3::new(self.next() - 0.5, self.next() - 0.5, self.next() - 0.5);
        let car = &mut cars[driver];

        if car.shield > 0.0 {
            car.shield = 0.0;
            self.events.push(Event::Blocked { driver, pos: car.pos });
            return;
        }

        let away = (car.pos - from).normalize_or(car.up());
        car.vel += away * (HIT_IMPULSE * scale / crate::vehicle::MASS);
        car.ang_vel += spin.normalize_or(Vec3::Y) * (HIT_SPIN * scale);
        car.stun = car.stun.max(STUN_TIME * scale);
        self.events.push(Event::Struck { driver, pos: car.pos });
    }
}

/// Crates along the track, on the floor, spaced so a lap offers a steady supply
/// without the field being permanently armed.
fn place_pickups(track: &Track) -> Vec<Pickup> {
    let n = track.frames.len();
    let spacing = ((PICKUP_SPACING_M / (track.length / n as f32)) as usize).max(4);
    // Offset from the boost pads so a crate and a pad are not the same patch of
    // road, where one would hide the other.
    let clear_of_start = (90.0 / (track.length / n as f32)) as usize;

    let mut pickups = Vec::new();
    let mut i = clear_of_start;
    while i < n - clear_of_start.min(n / 2) {
        let index = (track.start + i) % n;
        let f = &track.frames[index];
        // Straight down in world terms, flattened into the ring: a crate on an
        // open stretch belongs on the floor, not up the banking.
        let floor = (Vec3::NEG_Y - f.tangent * Vec3::NEG_Y.dot(f.tangent)).normalize_or(f.normal);
        pickups.push(Pickup {
            pos: f.pos + floor * (f.radius - 1.4),
            up: -floor,
            frame: index,
            cooldown: 0.0,
        });
        i += spacing;
    }
    pickups
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::{Sim, TICK_DT};
    use crate::vehicle::{autopilot, Controls};

    /// Race the field for a while and hand back the sim.
    fn race(seed: u64, seconds: f32, fire: bool) -> Sim {
        let mut sim = Sim::new(seed);
        for _ in 0..(seconds / TICK_DT) as usize {
            let mut controls = autopilot(&sim.track, &sim.player, 26.0);
            controls.fire = fire;
            sim.tick(&controls, TICK_DT);
        }
        sim
    }

    /// Crates have to be on the road, or they cannot be driven through.
    #[test]
    fn pickups_sit_on_the_driving_surface() {
        for seed in [1u64, 7, 42] {
            let track = Track::generate(seed);
            let pickups = place_pickups(&track);
            assert!(pickups.len() > 4, "seed {seed}: only {} crates on a lap", pickups.len());

            let mut hint = 0;
            for pickup in &pickups {
                let surface = track.surface(pickup.pos, hint);
                hint = surface.index;
                assert!(
                    surface.gap > 0.5 && surface.gap < 4.0,
                    "seed {seed}: a crate sits {:.2} m from the wall",
                    surface.gap
                );
            }
        }
    }

    /// Driving a lap has to actually arm the field, or none of the rest of this
    /// matters.
    #[test]
    fn racing_collects_weapons() {
        let sim = race(7, 40.0, false);
        // Nobody fired, so what is held plus what was spent is what was picked
        // up; with fire off, everything picked up is still held.
        let armed = sim.weapons.held.iter().filter(|h| h.is_some()).count();
        assert!(armed > 0, "nobody collected a weapon in forty seconds of racing");
    }

    /// The whole point of the weighting: the leader must not be drawing the
    /// same things as the back of the field.
    #[test]
    fn the_leader_draws_defence_and_the_back_draws_attack() {
        let track = Track::generate(7);
        let mut arsenal = Arsenal::new(&track, 7, 6);
        let mut leader = [0usize; 4];
        let mut last = [0usize; 4];
        let slot = |w: Weapon| match w {
            Weapon::Rocket => 0,
            Weapon::Shock => 1,
            Weapon::Mine => 2,
            Weapon::Shield => 3,
        };
        for _ in 0..4000 {
            leader[slot(arsenal.roll(0, 6))] += 1;
            last[slot(arsenal.roll(5, 6))] += 1;
        }
        let attack = |c: &[usize; 4]| c[0] + c[1];
        assert!(
            attack(&last) > attack(&leader) * 2,
            "the back of the field drew {} attacking items against the leader's {}",
            attack(&last),
            attack(&leader)
        );
        assert!(leader[3] > last[3], "the leader did not draw more shields");
        // Every item has to be reachable from somewhere, or it is dead content.
        for slot in 0..4 {
            assert!(leader[slot] + last[slot] > 0, "item {slot} is never drawn");
        }
    }

    /// A hit costs the driver their engine and their line, and nothing else.
    /// If it could end a race it would not belong in this game.
    #[test]
    fn a_hit_stuns_and_shoves_but_leaves_the_car_racing() {
        let mut sim = Sim::new(7);
        for _ in 0..(3.0 / TICK_DT) as usize {
            let controls = autopilot(&sim.track, &sim.player, 26.0);
            sim.tick(&controls, TICK_DT);
        }
        let before = sim.player.vel;
        let at = sim.player.pos + sim.player.forward() * 4.0;
        {
            let mut cars: Vec<&mut Vehicle> = vec![&mut sim.player];
            sim.weapons.strike(&mut cars, 0, at, 1.0);
        }
        assert!(sim.player.stun > 0.0, "a hit did not stun");
        assert!(sim.player.vel != before, "a hit did not move the car");
        assert!(sim.player.vel.is_finite(), "a hit produced a non-finite velocity");
        assert_eq!(sim.weapons.events.len(), 1);
        assert!(matches!(sim.weapons.events[0], Event::Struck { driver: 0, .. }));

        // And it wears off: drive on and the car recovers.
        for _ in 0..(4.0 / TICK_DT) as usize {
            let controls = autopilot(&sim.track, &sim.player, 26.0);
            sim.tick(&controls, TICK_DT);
        }
        assert_eq!(sim.player.stun, 0.0, "the stun never wore off");
        assert!(sim.player.speed() > 20.0, "the car never got going again after a hit");
    }

    /// A shield eats exactly one hit and is then gone.
    #[test]
    fn a_shield_absorbs_one_hit_and_only_one() {
        let mut sim = Sim::new(7);
        sim.player.shield = SHIELD_TIME;
        let at = sim.player.pos + Vec3::X * 5.0;

        {
            let mut cars: Vec<&mut Vehicle> = vec![&mut sim.player];
            sim.weapons.strike(&mut cars, 0, at, 1.0);
        }
        assert_eq!(sim.player.stun, 0.0, "a shielded car was stunned anyway");
        assert_eq!(sim.player.shield, 0.0, "the shield was not spent");
        assert!(matches!(sim.weapons.events[0], Event::Blocked { driver: 0, .. }));

        {
            let mut cars: Vec<&mut Vehicle> = vec![&mut sim.player];
            sim.weapons.strike(&mut cars, 0, at, 1.0);
        }
        assert!(sim.player.stun > 0.0, "the second hit was blocked too");
    }

    /// A rocket has to find a car ahead and hit it, and must never come back
    /// for the driver who fired it.
    ///
    /// Staged with two cars rather than raced, because in a real race an
    /// opponent wanders into range on its own and the rocket landing proves
    /// nothing about the homing.
    #[test]
    fn a_rocket_chases_a_car_ahead_and_never_its_owner() {
        let track = Track::generate(7);
        let mut shooter = Vehicle::new(&track, 0);
        let mut target = Vehicle::new(&track, 0);

        // Put the target on the road well ahead and off to one side, so only
        // homing can close the gap: fired straight, the rocket misses. Placed
        // through the track frames rather than by offsetting in world space,
        // which puts it through the wall.
        let spacing = track.length / track.frames.len() as f32;
        let ahead = (shooter.hint + (90.0 / spacing) as usize) % track.frames.len();
        let f = &track.frames[ahead];
        let floor = (Vec3::NEG_Y - f.tangent * Vec3::NEG_Y.dot(f.tangent)).normalize_or(f.normal);
        target.pos = f.pos + floor * (f.radius - 1.6) + f.tangent.cross(floor) * 8.0;
        target.hint = ahead;

        let mut arsenal = Arsenal::new(&track, 7, 2);
        arsenal.held[0] = Some(Weapon::Rocket);

        let mut struck = Vec::new();
        let mut closest_to_owner = f32::MAX;
        let mut fired = false;
        for step in 0..(6.0 / TICK_DT) as usize {
            {
                let mut cars: Vec<&mut Vehicle> = vec![&mut shooter, &mut target];
                arsenal.update(&track, &mut cars, &[step == 0], &[0.0, 90.0], TICK_DT);
            }
            fired |= !arsenal.rockets.is_empty();
            for rocket in &arsenal.rockets {
                assert!(rocket.pos.is_finite(), "a rocket went non-finite");
                assert!(
                    track.surface(rocket.pos, 0).gap > -1.0,
                    "a rocket left the tube"
                );
                if rocket.owner == 0 {
                    closest_to_owner = closest_to_owner.min(rocket.pos.distance(shooter.pos));
                }
            }
            struck.extend(arsenal.events.iter().filter_map(|e| match e {
                Event::Struck { driver, .. } => Some(*driver),
                _ => None,
            }));
        }

        assert!(fired, "firing a held rocket produced no rocket");
        assert_eq!(struck, vec![1], "the rocket struck {struck:?}, expected only the car ahead");
        assert!(
            closest_to_owner > 3.0,
            "a rocket came back within {closest_to_owner:.1} m of the car that fired it"
        );
    }

    /// The seeker itself, isolated from everything else that bends a rocket.
    ///
    /// Both cars are put near the tube axis so the wall standoff never fires:
    /// following the tube is enough to hit a car sitting on the racing line,
    /// so an outcome test cannot tell homing apart from that. Here the only
    /// thing that can turn the rocket is the seeker.
    #[test]
    fn the_seeker_turns_the_rocket_toward_a_target_off_its_nose() {
        let track = Track::generate(7);
        let spacing = track.length / track.frames.len() as f32;
        let mut shooter = Vehicle::new(&track, 0);
        let mut target = Vehicle::new(&track, 0);

        let here = &track.frames[shooter.hint];
        let ahead = &track.frames[(shooter.hint + (60.0 / spacing) as usize) % track.frames.len()];
        shooter.pos = here.pos;
        shooter.rot = crate::track::look_rotation(here.tangent, here.normal);
        // Well off the nose, and out in open tube where nothing else acts.
        target.pos = ahead.pos + here.normal * 14.0;

        let mut arsenal = Arsenal::new(&track, 7, 2);
        arsenal.held[0] = Some(Weapon::Rocket);

        let mut closest = f32::MAX;
        for step in 0..180 {
            {
                let mut cars: Vec<&mut Vehicle> = vec![&mut shooter, &mut target];
                arsenal.update(&track, &mut cars, &[step == 0], &[0.0, 60.0], TICK_DT);
            }
            let Some(rocket) = arsenal.rockets.first() else { continue };
            let range = rocket.pos.distance(target.pos);
            // The engagement ends at closest approach. What the rocket does
            // afterwards - come round for another pass, run into a wall - is
            // not what this is measuring.
            if range > closest + 5.0 {
                break;
            }
            closest = closest.min(range);
            // The standoff must not be what is doing the work.
            assert!(
                track.surface(rocket.pos, 0).gap > ROCKET_STANDOFF,
                "the rocket reached the wall, so this is no longer a test of the seeker"
            );
        }

        assert!(closest < f32::MAX, "no rocket was fired");
        // Fired straight from here the rocket passes about 14 m wide, so this
        // can only be met by the seeker having turned it.
        assert!(
            closest < 7.0,
            "the seeker left the rocket {closest:.1} m wide of a target 14 m off its nose"
        );
    }

    /// A dropped mine must not go off under the car that dropped it.
    ///
    /// Run with one car and no field. In a real race the player is being shot
    /// at by five other drivers, so "was the player hit?" answers a different
    /// question than the one being asked here.
    #[test]
    fn a_mine_does_not_catch_the_driver_who_dropped_it() {
        let track = Track::generate(7);
        let mut car = Vehicle::new(&track, 0);
        let mut arsenal = Arsenal::new(&track, 7, 1);
        arsenal.held[0] = Some(Weapon::Mine);

        // Up to racing speed first: a mine dropped by a car that is barely
        // moving is one it is entitled to sit on.
        for _ in 0..(5.0 / TICK_DT) as usize {
            let controls = autopilot(&track, &car, 26.0);
            car.step(&track, &controls, TICK_DT);
        }
        assert!(car.speed() > 20.0, "the car never got up to speed");

        for step in 0..(4.0 / TICK_DT) as usize {
            let mut controls = autopilot(&track, &car, 26.0);
            controls.fire = step == 0;
            car.step(&track, &controls, TICK_DT);
            {
                let mut cars: Vec<&mut Vehicle> = vec![&mut car];
                arsenal.update(&track, &mut cars, &[controls.fire], &[0.0], TICK_DT);
            }
            assert_eq!(car.stun, 0.0, "a driver was caught by their own mine");
        }
        assert!(!arsenal.mines.is_empty(), "the mine was never dropped");
    }

    /// But an armed mine does catch someone who drives into it, or it is not a
    /// trap at all.
    #[test]
    fn an_armed_mine_catches_a_car_that_drives_into_it() {
        let track = Track::generate(7);
        let mut car = Vehicle::new(&track, 0);
        let mut arsenal = Arsenal::new(&track, 7, 1);
        arsenal.mines.push(Mine {
            pos: car.pos,
            up: car.up(),
            arm: 0.0,
            life: MINE_LIFE,
        });

        let mut cars: Vec<&mut Vehicle> = vec![&mut car];
        arsenal.update(&track, &mut cars, &[false], &[0.0], TICK_DT);
        assert!(car.stun > 0.0, "an armed mine did not go off under a car");
        assert!(arsenal.mines.is_empty(), "the mine survived going off");
    }

    /// The shock reaches the car one place ahead and nobody else, and the
    /// leader cannot use one at all.
    #[test]
    fn a_shock_hits_the_car_one_place_ahead() {
        let mut sim = Sim::new(7);
        for _ in 0..(8.0 / TICK_DT) as usize {
            let controls = autopilot(&sim.track, &sim.player, 26.0);
            sim.tick(&controls, TICK_DT);
        }

        let place = sim.player_position();
        sim.weapons.held[0] = Some(Weapon::Shock);
        sim.weapons.cooldowns[0] = 0.0;

        let mut controls = autopilot(&sim.track, &sim.player, 26.0);
        controls.fire = true;
        sim.tick(&controls, TICK_DT);

        if place == 1 {
            // Leading: the shock has no target and must be a no-op rather than
            // hitting the player or panicking.
            assert_eq!(sim.player.stun, 0.0);
        } else {
            let struck: Vec<usize> = sim
                .weapons
                .events
                .iter()
                .filter_map(|e| match e {
                    Event::Struck { driver, .. } => Some(*driver),
                    _ => None,
                })
                .collect();
            assert_eq!(struck.len(), 1, "a shock struck {} cars", struck.len());
            assert_ne!(struck[0], 0, "a shock struck the driver who used it");
        }
    }

    /// A full race with everyone armed has to stay bounded and finite: no
    /// growing list of rockets, no stuck mines, no divergence.
    #[test]
    fn a_long_armed_race_stays_bounded() {
        let mut sim = race(7, 120.0, true);
        assert!(sim.weapons.rockets.len() < 64, "{} rockets live", sim.weapons.rockets.len());
        assert!(sim.weapons.mines.len() < 64, "{} mines live", sim.weapons.mines.len());
        assert!(sim.player.pos.is_finite() && sim.player.vel.is_finite());
        for opponent in &sim.opponents {
            assert!(opponent.car.pos.is_finite(), "an opponent diverged");
        }
        // The field still raced: weapons must not have brought it to a halt.
        assert!(sim.progress > 500.0, "the field only covered {:.0} m", sim.progress);

        // And the crates come back, so a long race does not run dry.
        let live = sim.weapons.pickups.iter().filter(|p| p.cooldown <= 0.0).count();
        assert!(live > 0, "every crate was collected and none respawned");

        let mut controls = Controls::default();
        controls.fire = true;
        sim.tick(&controls, TICK_DT);
    }

    /// Same seed, same race. Weapons are seeded from the race, so a replay is
    /// a replay and a bug is reproducible.
    #[test]
    fn an_armed_race_is_deterministic() {
        let a = race(11, 30.0, true);
        let b = race(11, 30.0, true);
        assert_eq!(a.weapons.rockets.len(), b.weapons.rockets.len());
        assert_eq!(a.weapons.held, b.weapons.held);
        assert!((a.player.pos - b.player.pos).length() < 1e-4, "the race diverged");
    }
}
