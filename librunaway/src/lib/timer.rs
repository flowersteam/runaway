//! lib/timer.rs
//!
//! This module contains a structure that allows asynchronous waits. It is based on the timing wheel
//! used in tokio-timer.


//------------------------------------------------------------------------------------------ IMPORTS


use crate::commons::Dropper;
use futures::Future;
use std::{error, fmt};
use futures::channel::{mpsc, oneshot};
use std::thread;
use futures::channel::mpsc::{UnboundedSender};
use std::fmt::Debug;
use crate::*;
use tracing::{self, warn, trace, instrument, trace_span};
use tracing_futures::Instrument;
use std::time::{Instant, Duration};
use arrayvec::ArrayVec;
use std::fmt::{Formatter};
use futures::sink::SinkExt;



//-------------------------------------------------------------------------------------------- CONST


/// The maximal amount of `Timed<T>` that can be stored in a `Slot<T>`.
const SLOT_LEN: usize = 100;
/// The number of slots contained in the wheel. Better be a power of 2.
const WHEEL_LEN: usize = 32;
/// The smallest slot duration of the timer.
const TIMER_PRECISION: usize =1;
/// The number of wheels used in the timer
const TIMER_LEN: usize = 3;


//------------------------------------------------------------------------------------------- ERRORS


#[derive(Debug, Clone)]
pub enum Error {
    Channel(String),
    OperationFetch(String),
    Shutdown,
}

impl error::Error for Error {}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::Channel(ref e) => write!(f, "A Channel related error occured: {}", e),
            Error::OperationFetch(ref e) => write!(f, "An error occured while fetching operation: {}", e),
            Error::Shutdown => write!(f, "The timer is shutdown"),
        }
    }
}


//-------------------------------------------------------------------------------------------- TIMED

/// A time-stamped container
#[derive(PartialEq)]
struct Timed<T>{
    /// The timestamp
    instant: Instant,
    /// The actual object
    object: T
}

// Debug could be unimplemented for `T`, so we don`t show it in the Debug format.
impl<T> Debug for Timed<T>{
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        write!(f, "Timed@{:?}{{...}}", self.instant)
    }
}


//--------------------------------------------------------------------------------------------- SLOT


/// Represent a fixed range time slot, which can store `Timed<T>` whose timestamps are contained
/// within the range
struct Slot<T>{
    /// Beginning of time slot. This instant is contained in the slot.
    beginning: Instant,
    /// End of time slot. This instant is __not__ contained in the slot itself.
    end: Instant,
    /// The list of `Timed<T>` stored in this timeslot.
    waiters: ArrayVec<[Timed<T>; SLOT_LEN]>,
}

impl<T> Debug for Slot<T> {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        write!(f, "Slot[{:?}->{:?}]{{{}/{}}}",
            self.beginning,
            self.end,
            self.waiters.len(),
            SLOT_LEN)
    }
}

impl<T> PartialEq for Slot<T> {
    fn eq(&self, other: &Self) -> bool {
        self.beginning == other.beginning
    }
}

impl<T> Slot<T>{

    /// Creates a new time slot.
    pub fn new(beginning: Instant, end: Instant) -> Slot<T>{
        Slot{
             beginning,
             end,
             waiters: ArrayVec::<[Timed<T>; SLOT_LEN]>::new(),
        }
    }

    /// Tries to insert a timed object. Returns `None` if the object was inserted (the timestamp
    /// was in the range of the slot), and `Some(timed)` if it was not (the timestamp was out
    /// of range).
    pub fn try_insert(&mut self, Timed{instant, object}: Timed<T>) -> Option<Timed<T>>{
        if self.contains_instant(&instant){
            self.waiters.push(Timed{instant, object});
            None
        } else {
            Some(Timed{instant, object})
        }
    }

    /// Checks whether an instant is before the slot.
    #[inline]
    pub fn is_instant_before(&self, instant: &Instant) -> bool{
        instant < &self.beginning
    }

    /// Checks whether an instant is after the slot.
    #[inline]
    pub fn is_instant_after(&self, instant: &Instant) -> bool{
        &self.end <= instant
    }

    /// Checks whether the instant lies in the slot.
    #[inline]
    pub fn contains_instant(&self, instant: &Instant) -> bool{
        !self.is_instant_before(instant) && !self.is_instant_after(instant)
    }

    /// Turns the slot to a vector.
    #[inline]
    pub fn to_vec(self) -> Vec<Timed<T>>{
        self.waiters.into_iter().collect()
    }

    /// Gives a handle to the inner.
    #[inline]
    #[allow(dead_code)]
    pub fn as_arrayvec(&self) -> &ArrayVec<[Timed<T>; SLOT_LEN]>{
        &self.waiters
    }

    /// Returns the beginning instant.
    #[inline]
    pub fn beginning(&self) -> Instant {
        self.beginning
    }

    /// Returns the end instant.
    #[inline]
    pub fn end(&self) -> Instant {
        self.end
    }

    /// Returns the duration of the slot.
    #[inline]
    #[allow(dead_code)]
    pub fn duration(&self) -> Duration{
        self.end-self.beginning
    }

    /// Returns the number of timed inside.
    #[inline]
    #[allow(dead_code)]
    pub fn len(&self) -> usize{
        self.waiters.len()
    }
}


//-------------------------------------------------------------------------------------------- WHEEL


/// An ordered set of fixed-duration time slots, with expiration mechanism.
struct Wheel<T>{
    slots: ArrayVec<[Slot<T>; WHEEL_LEN]>,
    increment: Duration,
}

impl<T> std::fmt::Debug for Wheel<T>{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Wheel@{:?}[{:#?}] ", self.increment, self.slots)
    }
}

impl<T> Wheel<T>{

    /// Creates a new wheel from its genesis and increment
    pub fn new(from: Instant, increment: Duration) -> Wheel<T>{
        let mut slots = ArrayVec::<[Slot<T>; WHEEL_LEN]>::new();
        let mut from = from;
        for _ in 0..WHEEL_LEN{
            slots.push(Slot::new(from, from+increment));
            from += increment;
        }
        Wheel{
            slots,
            increment,
        }
    }

    /// Pushes a timed into the wheel. If successful the function returns the index of the slot it
    /// was pushed in. If not, it returns the timed.
    pub fn try_insert(&mut self, timed: Timed<T>) -> Result<usize, Timed<T>>{
        self.slots.iter_mut()
            .enumerate()
            .fold(Err(timed), |acc, (i, s)|{
                match acc{
                    Err(t) => s.try_insert(t).map_or(Ok(i), |t| Err(t)),
                    Ok(s) => Ok(s)
                }
            })
    }

    /// Returns true, if the wheel should turn regarding the time given as input.
    #[inline]
    pub fn should_turn(&self, now: &Instant) -> bool{
         self.slots[0].is_instant_after(now)
    }

    /// Removes and return the lower slot and pushes a new one at the end.
    pub fn turn(&mut self) -> Slot<T>{
        let slot = self.slots.remove(0);
        let last = self.slots[WHEEL_LEN-2].end;
        let next_slot = Slot::new(last, last+self.increment);
        self.slots.push(next_slot);
        slot
    }

    /// Returns the beginning instant
    #[inline]
    pub fn beginning(&self) -> Instant {
        self.slots[0].beginning
    }

    /// Returns the end instant.
    #[inline]
    pub fn end(&self) -> Instant {
        self.slots[WHEEL_LEN-1].end
    }

    /// Returns the duration.
    #[inline]
    pub fn duration(&self) -> Duration{
        self.end()-self.beginning()
    }

    /// Returns the number of timed stored.
    #[inline]
    pub fn len(&self) -> usize{
        self.slots.iter().fold(0, |a, s| a+s.len())
    }
}

//-------------------------------------------------------------------------------------------- TIMER


/// A timer using a hierarchy of timing wheel.
struct Timer<T>(ArrayVec<[Wheel<T>; TIMER_LEN]>);

impl<T> Debug for Timer<T>{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Timer[{:#?}] ", self.0)
    }
}

impl<T> Timer<T>{

    /// Creates a new timer.
    pub fn new(beginning: Instant) -> Timer<T>{
        let mut wheels: ArrayVec<[Wheel<T>; TIMER_LEN]> = ArrayVec::<[Wheel<T>; TIMER_LEN]>::new();
        let mut beginning = beginning;
        let mut increment = Duration::from_millis(TIMER_PRECISION as u64);
        for _ in 0..TIMER_LEN{
            let wheel: Wheel<T> = Wheel::new(beginning, increment);
            beginning = wheel.end();
            increment = wheel.duration();
            wheels.push(wheel);
        }
        Timer(wheels)
    }

    /// Pushes a timed into the timer. If successful the function returns the index of the wheel it
    /// was pushed in. If not, it returns the timed.
    pub fn try_insert(&mut self, timed: Timed<T>) -> Result<usize, Timed<T>>{
        self.0.iter_mut()
            .enumerate()
            .fold(Err(timed), |acc, (i, w)|{
                match acc{
                    Err(t) => w.try_insert(t).map(|_| i),
                    Ok(s) => Ok(s)
                }
            })
    }

    /// Return true, if the timer should turn regarding the time given as input.
    pub fn should_turn(&self, now: &Instant) -> bool{
        self.0[0].should_turn(now)
    }

    /// Turns the wheels and yields the expired `Timed<T>`.
    pub fn turn(&mut self) -> Vec<T>{
        // We turn the high-frequency wheel
        let ret = self.0[0].turn();
        // For all the lower-frequency wheels, we check if they should be turned, in which case
        // the gathered timed are forwarded to the previous wheel.
        for w in 1..TIMER_LEN{
            let do_turn = {
                let previous_wheel = &self.0[w-1];
                let previous_span = (previous_wheel.beginning(), previous_wheel.end());
                let current_slot = &self.0[w].slots[0];
                let current_span = (current_slot.beginning(), current_slot.end());
                current_span == previous_span
            };

            if do_turn {
                let slot = {
                    let current_wheel = &mut self.0[w];
                    current_wheel.turn()
                };
                let previous_wheel = &mut self.0[w-1];
                let res: Result<Vec<usize>, Timed<T>> = slot.to_vec()
                    .into_iter()
                    .map(|t| previous_wheel.try_insert(t))
                    .collect();
                res.expect("Failed to transfer entities from wheel to wheel");
            } else {
                break
            }
        }
        ret.waiters.into_iter().map(|Timed{object, ..}| object).collect()
    }

    /// Sleeps until the next expiration instant.
    pub fn sleep_until_next(&self){
        let next = self.0[0].slots[0].end();
        let now = Instant::now();
        if next > now{
            std::thread::sleep(next-now);
        }
    }

    /// Returns the number of timed in the timer.
    #[allow(dead_code)]
    pub fn len(&self) -> usize{
        self.0.iter().fold(0, |a, w| a+w.len())
    }
}


//------------------------------------------------------------------------------------------- HANDLE

#[derive(Debug)]
/// Messages sent by the outer future to the resource inner thread, so as to start an operation.
/// This contains the input of the operation if any.
enum OperationInput{
    Sleep(Instant),
}

#[derive(Debug)]
/// Messages sent by the inner future to the outer future, so as to return the result of an
/// operation.
enum OperationOutput{
    Sleep,
}

#[derive(Clone)]
/// An asynchronous handle to the scheduler resource.
pub struct TimerHandle {
    _sender: UnboundedSender<(oneshot::Sender<OperationOutput>, OperationInput)>,
    _dropper: Dropper,
}

impl TimerHandle{

    #[instrument(name="TimerHandle::spawn")]
    pub fn spawn() -> Result<TimerHandle, Error> {

        trace!("Start timer thread");
        // We create the channel that will be used to transmit operations from the outer logic (when
        // the user call one of the async api methods) to the inner handling thread.
        let (sender, mut receiver) = mpsc::unbounded();

        // We spawn the thread that dispatches the operations sent by the different handles to inner
        // futures.
        let handle = thread::Builder::new().name("orch-timer".into()).spawn(move || {
            let span = trace_span!("Timer::Thread");
            let _guard = span.enter();
            let mut timer = Timer::new(Instant::now());
            trace!("Consuming timers");
            'timer: loop{
                if timer.should_turn(&Instant::now()){
                    let fs = timer.turn();
                    fs.into_iter()
                        .for_each(|s: oneshot::Sender<OperationOutput>| {
                            s.send(OperationOutput::Sleep).unwrap()
                        });

                }

                'waiters: loop{
                    match receiver.try_next(){
                        Ok(Some((s, OperationInput::Sleep(i)))) => {
                            let t = Timed{instant: i, object: s};
                            match timer.try_insert(t) {
                                Err(s) => {
                                    warn!("A timer came in already timed out.");
                                    s.object.send(OperationOutput::Sleep).unwrap()
                                },
                                Ok(_) => {}
                            };
                        }
                        Err(_) => {
                            break 'waiters
                        }
                        Ok(None) => {
                            break 'timer
                        }
                    }
                }

                timer.sleep_until_next();
            }
            trace!("All good. Leaving...");
        }).expect("Failed to spawn timer thread.");

        // We return the handle
        let drop_sender = sender.clone();
        Ok(TimerHandle{
            _sender: sender,
            _dropper: Dropper::from_closure(
                Box::new(move || {
                    drop_sender.close_channel();
                    handle.join().unwrap();
                }),
                format!("TimerHandle")
            ),
        })
    }

    /// Async method, which request a parameter string from the scheduler, and wait for it if the
    /// scheduler is not yet ready.
    pub fn async_sleep(&self, dur: Duration) -> impl Future<Output=Result<(), Error>> {
        let mut chan = self._sender.clone();
        async move {
            let (sender, receiver) = oneshot::channel();
            trace!("Sending async sleep parameters input");
            chan.send((sender, OperationInput::Sleep(Instant::now()+dur)))
                .await
                .map_err(|e| Error::Channel(e.to_string()))?;
            trace!("Awaiting async request parameters output");
            match receiver.await {
                Err(e) => Err(Error::OperationFetch(format!("{}", e))),
                Ok(OperationOutput::Sleep) => Ok(()),
            }
        }.instrument(trace_span!("SchedulerHandle::async_request_parameters"))
    }
}

impl Debug for TimerHandle{
    fn fmt(&self, f: &mut std::fmt::Formatter) -> Result<(), std::fmt::Error>{
        write!(f, "TimerHandle",)
    }
}


//-------------------------------------------------------------------------------------------- TESTS


#[cfg(test)]
mod test {

    use super::*;
    use futures::executor::block_on;

    #[test]
    fn test_slot(){
        let beginning = Instant::now();
        let end = beginning+Duration::from_secs(10);
        let mut s: Slot<()> = Slot::new(beginning, end);
        dbg!(&s);
        assert!(s.try_insert(Timed{instant: beginning, object: ()}).is_none(), "Failed to add beginning");
        assert!(s.try_insert(Timed{instant: end, object: ()}).is_some(), "Failed not to add end");
        let before = beginning - Duration::from_secs(1);
        let during = beginning + Duration::from_secs(5);
        let after = end + Duration::from_secs(11);
        assert!(s.try_insert(Timed{instant: before, object: ()}).is_some(), "Failed not to add before");
        assert!(s.try_insert(Timed{instant: during, object: ()}).is_none(), "Failed to add during");
        assert!(s.try_insert(Timed{instant: after, object: ()}).is_some(), "Failed not to add after");
        dbg!(&s);
        let v = s.to_vec();
        assert_eq!(v, vec!(Timed{instant: beginning, object: ()}, Timed{instant: during, object: ()}), "Failed to produce right vector");
    }

    #[test]
    fn test_wheel(){
        let beginning = Instant::now();
        let mut wheel: Wheel<usize> = Wheel::new(beginning, Duration::from_millis(1));
        dbg!(&wheel);
        let t = Timed{instant: beginning+Duration::from_millis(1), object:0};
        assert_eq!(wheel.try_insert(t).unwrap(), 1);
        dbg!(&wheel);
        let t = Timed{instant: beginning+Duration::from_micros(3000), object:0};
        assert_eq!(wheel.try_insert(t).unwrap(), 3);
        dbg!(&wheel);
        let t = Timed{instant: beginning+Duration::from_micros(2999), object:0};
        assert_eq!(wheel.try_insert(t).unwrap(), 2);
        dbg!(&wheel);
        let t = Timed{instant: beginning+Duration::from_micros(32000), object:0};
        assert!(wheel.try_insert(t).is_err());
        dbg!(&wheel);
        let now = beginning+Duration::from_micros(500);
        assert!(!wheel.should_turn(&now));
        let now = beginning+Duration::from_micros(1000);
        assert!(wheel.should_turn(&now));
        let s = wheel.turn();
        dbg!(&s);
        dbg!(&wheel);
        let now = beginning+Duration::from_micros(3000);
        assert!(wheel.should_turn(&now));
        let s = wheel.turn();
        dbg!(&s);
        dbg!(&wheel);
    }

    #[test]
    fn test_timer(){
        let beginning= Instant::now();
        let mut timer: Timer<()> = Timer::new(beginning);
        let instant = beginning+Duration::from_millis(3);
        assert_eq!(timer.try_insert(Timed{instant, object:()}).unwrap(), 0);
        assert!(timer.0[0].len()==1);
        let instant = beginning+Duration::from_millis(32);
        assert_eq!(timer.try_insert(Timed{instant, object:()}).unwrap(), 1);
        assert!(timer.0[1].len()==1);
        let instant = beginning+Duration::from_millis(1056);
        assert_eq!(timer.try_insert(Timed{instant, object:()}).unwrap(), 2);
        assert!(timer.0[2].len()==1);

        let mut now = beginning;
        assert!(!timer.should_turn(&now));
        now += Duration::from_millis(1);
        assert!(timer.should_turn(&now));
        assert_eq!(timer.turn().len(), 0);
        now += Duration::from_millis(1);
        assert!(timer.should_turn(&now));
        assert_eq!(timer.turn().len(), 0);
        now += Duration::from_millis(1);
        assert!(timer.should_turn(&now));
        assert_eq!(timer.turn().len(), 0);
        now += Duration::from_millis(1);
        assert!(timer.should_turn(&now));
        assert_eq!(timer.turn().len(), 1);
    }

    #[test]
    fn test_timer_handle(){
        let  timer_handle = TimerHandle::spawn().unwrap();
        for i in 2..150{
            let now = Instant::now();
            let dur = Duration::from_millis(i*10);
            block_on(timer_handle.async_sleep(dur)).unwrap();
            let rdur = Instant::now() - now;
            println!("Supposed to wait {:?}, waited {:?}", dur, rdur);
            assert!(rdur>dur);
        }
    }
}
