use bevy_utils::HashMap;
use std::marker::PhantomData;

use crate::schedule::SystemLabel;
use crate::system::{Command, IntoSystem, System, SystemTypeIdLabel};
use crate::world::{Mut, World};

/// Stores initialized [`Systems`](crate::system::System), so they can be reused and run in an ad-hoc fashion
///
/// Systems are keyed by their [`SystemLabel`]:
///  - all systems with a given label will be run (in linear registration order) when a given label is run
///  - repeated calls with the same function type will reuse cached state, including for change detection
///
/// Any [`Commands`](crate::system::Commands) generated by these systems (but not other systems), will immediately be applied.
///
/// This type is stored as a [`Resource`](crate::system::Resource) on each [`World`], initialized by default.
/// However, it will likely be easier to use the corresponding methods on [`World`],
/// to avoid having to worry about split mutable borrows yourself.
///
/// # Limitations
///
///  - stored systems cannot be chained: they can neither have an [`In`](crate::system::In) nor return any values
///  - stored systems cannot recurse: they cannot run other systems via the [`SystemRegistry`] methods on `World` or `Commands`
///  - exclusive systems cannot be used
///
/// # Examples
///
/// You can run a single system directly on the World,
/// applying its effect and caching its state for the next time
/// you call this method (internally, this is based on [`SystemTypeIdLabel`]).
///
/// ```rust
/// use bevy_ecs::prelude::*;
///
/// let mut world = World::new();  
///
/// #[derive(Default, PartialEq, Debug)]
/// struct Counter(u8);
///
/// fn count_up(mut counter: ResMut<Counter>){
///     counter.0 += 1;
/// }
///
/// world.init_resource::<Counter>();
/// world.run_system(count_up);
///
/// assert_eq!(Counter(1), *world.resource());
/// ```
///
/// These systems immediately apply commands and cache state,
/// ensuring that change detection and [`Local`](crate::system::Local) variables work correctly.
///
/// ```rust
/// use bevy_ecs::prelude::*;
///
/// let mut world = World::new();
///
/// #[derive(Component)]
/// struct Marker;
///
/// fn spawn_7_entities(mut commands: Commands) {
///     for _ in 0..7 {
///         commands.spawn().insert(Marker);
///     }
/// }
///
/// fn assert_7_spawned(query: Query<(), Added<Marker>>){
///     let n_spawned = query.iter().count();
///     assert_eq!(n_spawned, 7);
/// }
///
/// world.run_system(spawn_7_entities);
/// world.run_system(assert_7_spawned);
/// ```
///
/// Systems can also be manually registered using [`SystemLabel`] types
/// and then run via those labels, enabling more sophisticated control flows.
///
/// ```rust
/// use bevy_ecs::prelude::*;
/// use bevy_ecs::system::SystemRegistry;
///
/// let mut world = World::new();
/// let mut system_registry = SystemRegistry::default();
///
/// #[derive(SystemLabel, Debug, PartialEq, Eq, Hash, Clone)]
/// enum ManualSystems {
///     Hello,
///     Goodbye,
/// }
///
/// fn hello(){
///     println!("Hello!")
/// }
///
/// fn goodbye(){
///     println!("Goodbye <3")
/// }
///
/// fn have_a_nice_day(){
///     println!("Have a nice day, and enjoy using Bevy!")
/// }
///
/// // You can register systems by their label
/// system_registry.register_system(&mut world, hello, ManualSystems::Hello);
///
/// // And run them by their label as well
/// system_registry.run_systems_by_label(&mut world, ManualSystems::Hello);
///
/// // You can register systems under multiple labels
/// system_registry.register_system_with_labels(&mut world, have_a_nice_day, [ManualSystems::Hello, ManualSystems::Goodbye]);
///
/// // All systems registered under that label will be run, in registration order
/// system_registry.run_systems_by_label(&mut world, ManualSystems::Hello);
///
/// // The methods on this type are also exposed on the `World` for convenience
/// world.register_system(goodbye, ManualSystems::Goodbye);
/// world.run_systems_by_label(ManualSystems::Goodbye);
/// ```
#[derive(Default)]
pub struct SystemRegistry {
    systems: Vec<StoredSystem>,
    // Stores the index of all systems that match the key's label
    labels: HashMap<Box<dyn SystemLabel>, Vec<usize>>,
}

struct StoredSystem {
    system: Box<dyn System<In = (), Out = ()>>,
}

impl SystemRegistry {
    /// Registers a system in the [`SystemRegistry`], so then it can be later run.
    ///
    /// Ordinarily, systems are automatically registered when [`run_system`](SystemRegistry::run_system) is called.
    /// However, manual registration allows you to provide one or more labels for the system.
    ///
    /// When [`run_systems_by_label`](SystemRegistry::run_systems_by_label) is called,
    /// all registered systems that match that label will be evaluated.
    ///
    /// To provide multiple labels, use [`register_system_with_labels`](SystemRegistry::register_system_with_labels).
    #[inline]
    pub fn register_system<Params, S: IntoSystem<(), (), Params> + 'static, L: SystemLabel>(
        &mut self,
        world: &mut World,
        system: S,
        label: L,
    ) {
        let boxed_system: Box<dyn System<In = (), Out = ()>> =
            Box::new(IntoSystem::into_system(system));

        self.register_boxed_system_with_labels(world, boxed_system, vec![Box::new(label)]);
    }

    /// Register system a system with any number of [`SystemLabel`]s.
    ///
    /// This allows the system to be run whenever any of its labels are run using [`run_systems_by_label`](SystemRegistry::run_systems_by_label).
    pub fn register_system_with_labels<
        Params,
        S: IntoSystem<(), (), Params> + 'static,
        LI: IntoIterator<Item = L>,
        L: SystemLabel,
    >(
        &mut self,
        world: &mut World,
        system: S,
        labels: LI,
    ) {
        let boxed_system: Box<dyn System<In = (), Out = ()>> =
            Box::new(IntoSystem::into_system(system));

        let collected_labels = labels
            .into_iter()
            .map(|label| {
                let boxed_label: Box<dyn SystemLabel> = Box::new(label);
                boxed_label
            })
            .collect();

        self.register_boxed_system_with_labels(world, boxed_system, collected_labels);
    }

    /// A more exacting version of [`register_system_with_labels`](Self::register_system_with_labels).
    ///
    /// This can be useful when you have a boxed system or boxed labels,
    /// as the corresponding traits are not implemented for boxed trait objects
    /// to avoid indefinite nesting.
    pub fn register_boxed_system_with_labels(
        &mut self,
        world: &mut World,
        mut boxed_system: Box<dyn System<In = (), Out = ()>>,
        labels: Vec<Box<dyn SystemLabel>>,
    ) {
        // Intialize the system's state
        boxed_system.initialize(world);

        let stored_system = StoredSystem {
            system: boxed_system,
        };

        // Add the system to the end of the vec
        self.systems.push(stored_system);
        let system_index = self.systems.len();

        // For each label that the system has
        for label in labels {
            let maybe_label_indexes = self.labels.get_mut(&label);

            // Add the index of the system in the vec to the lookup hashmap
            // under the corresponding label key
            if let Some(label_indexes) = maybe_label_indexes {
                label_indexes.push(system_index);
            } else {
                self.labels.insert(label, vec![system_index]);
            };
        }
    }

    /// Runs the system at the supplied `index` a single time
    #[inline]
    fn run_system_at_index(&mut self, world: &mut World, index: usize) {
        let stored_system = &mut self.systems[index];

        // Run the system
        stored_system.system.run((), world);
        // Apply any generated commands
        stored_system.system.apply_buffers(world);
    }

    /// Is at least one system in the [`SystemRegistry`] is associated with the provided [`SystemLabel`]?
    #[inline]
    pub fn is_label_registered<L: SystemLabel>(&self, label: L) -> bool {
        let boxed_label: Box<dyn SystemLabel> = Box::new(label);
        self.labels.get(&boxed_label).is_some()
    }

    /// Returns the first matching index for systems with this label
    ///
    /// # Panics
    ///
    /// Panics if no system with the label is registered.
    #[inline]
    fn first_registered_index<L: SystemLabel>(&self, label: L) -> usize {
        let boxed_label: Box<dyn SystemLabel> = Box::new(label);
        let vec_of_indexes = self.labels.get(&boxed_label).unwrap();
        *vec_of_indexes.iter().next().unwrap()
    }

    /// Runs the set of systems corresponding to the provided [`SystemLabel`] on the [`World`] a single time
    ///
    /// Systems will be run sequentially in registration order if more than one registered system matches the provided label
    pub fn run_systems_by_label<L: SystemLabel>(&mut self, world: &mut World, label: L) {
        let boxed_label: Box<dyn SystemLabel> = label.dyn_clone();
        self.run_systems_by_boxed_label(world, boxed_label);
    }

    /// A more exacting version of [`run_systems_by_label`](Self::run_systems_by_label).
    ///
    /// This can be useful when you have boxed labels,
    /// as [`SystemLabel`] is not implemented for boxed trait objects
    /// to avoid indefinite nesting.
    #[inline]
    fn run_systems_by_boxed_label(&mut self, world: &mut World, boxed_label: Box<dyn SystemLabel>) {
        let matching_system_indexes = self.labels.get(&boxed_label).unwrap_or_else(||{panic!{"No system with the `SystemLabel` {boxed_label:?} was found. Did you forget to register it?"}});

        // Loop over the system in registration order
        for index in matching_system_indexes.clone() {
            self.run_system_at_index(world, index);
        }
    }

    /// Runs the supplied system on the [`World`] a single time
    ///
    /// System state will be reused between runs, ensuring that [`Local`](crate::system::Local) variables and change detection works correctly.
    /// If, via manual system registration, you have somehow managed to insert more than one system with the same [`SystemTypeIdLabel`],
    /// only the first will be run.
    pub fn run_system<Params, S: IntoSystem<(), (), Params> + 'static>(
        &mut self,
        world: &mut World,
        system: S,
    ) {
        let automatic_system_label: SystemTypeIdLabel<S> = SystemTypeIdLabel::new();

        if !self.is_label_registered(automatic_system_label) {
            let boxed_system: Box<dyn System<In = (), Out = ()>> =
                Box::new(IntoSystem::into_system(system));
            let labels = boxed_system.default_labels();
            self.register_boxed_system_with_labels(world, boxed_system, labels);
        }
        self.run_system_at_index(world, self.first_registered_index(automatic_system_label));
    }
}

impl World {
    /// Registers the supplied system in the [`SystemRegistry`] resource
    ///
    /// This allows the system to be run by their [`SystemLabel`] using [`World::run_systems_by_label`].
    /// If you are using [`World::run_system`] directly, manual registration is not needed.
    /// The system will be automatically registered under its [`SystemTypeIdLabel`] the first time it is run.
    #[inline]
    pub fn register_system<Params, S: IntoSystem<(), (), Params> + 'static, L: SystemLabel>(
        &mut self,
        system: S,
        label: L,
    ) {
        self.resource_scope(|world, mut registry: Mut<SystemRegistry>| {
            registry.register_system(world, system, label);
        });
    }

    pub fn register_system_with_labels<
        Params,
        S: IntoSystem<(), (), Params> + 'static,
        LI: IntoIterator<Item = L>,
        L: SystemLabel,
    >(
        &mut self,
        system: S,
        labels: LI,
    ) {
        self.resource_scope(|world, mut registry: Mut<SystemRegistry>| {
            registry.register_system_with_labels(world, system, labels);
        });
    }

    /// Runs the supplied system on the [`World`] a single time
    ///
    /// Any [`Commands`](crate::system::Commands) generated will also be applied to the world immediately.
    ///
    /// The system's state will be cached: any future calls using the same type will use this state,
    /// improving performance and ensuring that change detection works properly.
    ///
    /// This is evaluated in a sequential, single-threaded fashion.
    /// Consider creating and running a [`Schedule`](crate::schedule::Schedule) if you need to execute large groups of systems
    /// at once, and want parallel execution of these systems.
    #[inline]
    pub fn run_system<Params, S: IntoSystem<(), (), Params> + 'static>(&mut self, system: S) {
        self.resource_scope(|world, mut registry: Mut<SystemRegistry>| {
            registry.run_system(world, system);
        });
    }

    /// Runs the system corresponding to the supplied [`SystemLabel`] on the [`World`] a single time
    ///
    /// Systems must be registered before they can be run by their label.
    ///
    /// Any [`Commands`](crate::system::Commands) generated will also be applied to the world immediately.
    ///
    /// The system's state will be cached: any future calls using the same type will use this state,
    /// improving performance and ensuring that change detection works properly.
    ///
    /// This is evaluated in a sequential, single-threaded fashion.
    /// Consider creating and running a [`Schedule`](crate::schedule::Schedule) if you need to execute large groups of systems
    /// at once, and want parallel execution of these systems.
    #[inline]
    pub fn run_systems_by_label<L: SystemLabel>(&mut self, label: L) {
        self.resource_scope(|world, mut registry: Mut<SystemRegistry>| {
            registry.run_systems_by_label(world, label);
        });
    }
}

/// The [`Command`] type for [`SystemRegistry::run_system`]
#[derive(Debug, Clone)]
pub struct RunSystemCommand<
    Params: Send + Sync + 'static,
    S: IntoSystem<(), (), Params> + Send + Sync + 'static,
> {
    _phantom_params: PhantomData<Params>,
    system: S,
}

impl<Params: Send + Sync + 'static, S: IntoSystem<(), (), Params> + Send + Sync + 'static>
    RunSystemCommand<Params, S>
{
    /// Creates a new [`Command`] struct, which can be added to [`Commands`](crate::system::Commands)
    #[inline]
    #[must_use]
    pub fn new(system: S) -> Self {
        Self {
            _phantom_params: PhantomData::default(),
            system,
        }
    }
}

impl<Params: Send + Sync + 'static, S: IntoSystem<(), (), Params> + Send + Sync + 'static> Command
    for RunSystemCommand<Params, S>
{
    #[inline]
    fn write(self, world: &mut World) {
        world.run_system(self.system);
    }
}

/// The [`Command`] type for [`SystemRegistry::run_systems_by_label`]
#[derive(Debug, Clone)]
pub struct RunSystemsByLabelCommand {
    pub label: Box<dyn SystemLabel>,
}

impl Command for RunSystemsByLabelCommand {
    #[inline]
    fn write(self, world: &mut World) {
        world.resource_scope(|world, mut registry: Mut<SystemRegistry>| {
            registry.run_systems_by_boxed_label(world, self.label.dyn_clone());
        });
    }
}

mod tests {
    use crate::prelude::*;

    #[derive(Default, PartialEq, Debug)]
    struct Counter(u8);

    #[allow(dead_code)]
    fn count_up(mut counter: ResMut<Counter>) {
        counter.0 += 1;
    }

    #[test]
    fn run_system() {
        let mut world = World::new();
        world.init_resource::<Counter>();
        assert_eq!(*world.resource::<Counter>(), Counter(0));
        world.run_system(count_up);
        assert_eq!(*world.resource::<Counter>(), Counter(1));
    }

    #[test]
    fn run_system_by_label() {
        let mut world = World::new();
        world.init_resource::<Counter>();
        assert_eq!(*world.resource::<Counter>(), Counter(0));
        world.register_system(count_up, "count");
        world.register_system(count_up, "count");
        world.run_systems_by_label("count");
        assert_eq!(*world.resource::<Counter>(), Counter(2));
    }

    #[allow(dead_code)]
    fn spawn_entity(mut commands: Commands) {
        commands.spawn();
    }

    #[test]
    fn command_processing() {
        let mut world = World::new();
        world.init_resource::<Counter>();
        assert_eq!(world.entities.len(), 0);
        world.run_system(spawn_entity);
        assert_eq!(world.entities.len(), 1);
    }

    fn non_send_count_down(mut ns: NonSendMut<Counter>) {
        ns.0 -= 1;
    }

    #[allow(dead_code)]
    fn trigger_non_send_count_down(mut commands: Commands) {
        commands.run_system(non_send_count_down);
    }

    #[test]
    fn non_send_resources() {
        let mut world = World::new();
        world.insert_non_send_resource(Counter(10));
        assert_eq!(*world.non_send_resource::<Counter>(), Counter(10));
        world.run_system(non_send_count_down);
        assert_eq!(*world.non_send_resource::<Counter>(), Counter(9));
        world.run_system(trigger_non_send_count_down);
        assert_eq!(*world.non_send_resource::<Counter>(), Counter(8));
    }

    #[derive(Default)]
    struct ChangeDetector;

    #[allow(dead_code)]
    fn count_up_iff_changed(mut commands: Commands, change_detector: ResMut<ChangeDetector>) {
        if change_detector.is_changed() {
            commands.run_system(count_up);
        }
    }

    #[test]
    fn change_detection() {
        let mut world = World::new();
        world.init_resource::<ChangeDetector>();
        world.init_resource::<Counter>();
        assert_eq!(*world.resource::<Counter>(), Counter(0));
        // Resources are changed when they are first added.
        world.run_system(count_up_iff_changed);
        assert_eq!(*world.resource::<Counter>(), Counter(1));
        // Nothing changed
        world.run_system(count_up_iff_changed);
        assert_eq!(*world.resource::<Counter>(), Counter(1));
        // Making a change
        world.resource_mut::<ChangeDetector>().set_changed();
        world.run_system(count_up_iff_changed);
        assert_eq!(*world.resource::<Counter>(), Counter(2));
    }

    #[allow(dead_code)]
    // The `Local` begins at the default value of 0
    fn fibonacci_counting(last_counter: Local<u8>, mut counter: ResMut<Counter>) {
        counter.0 += *last_counter;
    }

    #[test]
    fn local_variables() {
        let mut world = World::new();
        world.insert_resource(Counter(1));
        assert_eq!(*world.resource::<Counter>(), Counter(1));
        world.run_system(fibonacci_counting);
        assert_eq!(*world.resource::<Counter>(), Counter(1));
        world.run_system(fibonacci_counting);
        assert_eq!(*world.resource::<Counter>(), Counter(2));
        world.run_system(fibonacci_counting);
        assert_eq!(*world.resource::<Counter>(), Counter(3));
        world.run_system(fibonacci_counting);
        assert_eq!(*world.resource::<Counter>(), Counter(5));
    }

    #[allow(dead_code)]
    fn count_to_ten(mut counter: ResMut<Counter>, mut commands: Commands) {
        counter.0 += 1;
        if counter.0 < 10 {
            commands.run_system(count_to_ten);
        }
    }

    #[test]
    // This is a known limitation;
    // if this test passes the docs must be updated to reflect this
    // added functionality
    #[should_panic]
    fn system_recursion() {
        let mut world = World::new();
        world.init_resource::<Counter>();
        assert_eq!(*world.resource::<Counter>(), Counter(0));
        world.run_system(count_to_ten);
        assert_eq!(*world.resource::<Counter>(), Counter(10));
    }
}
