# runa - wayland compositor toolbox

## Demo

https://user-images.githubusercontent.com/366851/228325348-7b988f84-9837-4f5b-8fcd-d2085ea7edea.mp4

## Project status, goals and plans

This project is still pretty much a "tech demo" in this stage. What you can see above is all that works. And it took a lot of short cuts to even get that far. (And please don't run `cargo test`.)

Creating a wayland compositor library from the ground up is a daunting task, and I don't think I can do this just by myself. That's why this project is announced in this state - because I wanted to attract interests, and potentially collaborators to this project.

I tried to document the code base in its current state as best as I can. Have a look at the documentation, or the code base, and get in touch if you are interested:

- [runa-core](https://yshui.github.io/runa/runa_core/index.html): base traits and types for a wayland compositor.
- [apollo](https://yshui.github.io/runa/apollo/index.html): wayland protocol impls based on `runa-core`.

## Chat room

I am currently hanging out in the `#runa` channel on libra.chat.

## Roadmap

### 0.0.2

- Have unittests for easily testable components.
- All of the core and stable wayland interfaces implemented. No more "not implemented" error.
- Fix interface versioning

### 0.0.3 - 0.0.4

- Crescent able to run as DRM master
  - We likely want to create a crate for making accessing raw display devices and input devices easier.
    or use an existing crate. winit might become a viable option (rust-windowing/winit#2272)
- Support hardware accelerated clients

### 0.1.0

- Crescent can function as a minimal wayland compositor
- A tutorial for writing compositors with runa

### 0.2.0

- Xwayland support

## Q&A

- **Why runa, instead of other wayland libraries like wlroots, or smithay?**

  wlroots has a model that is not very suitable for a Rust binding - people has [tried about failed](https://way-cooler.org/blog/2019/04/29/rewriting-way-cooler-in-c.html) at creating a binding for it. And even if I do manage to create a wlroots binding, it's not going to be ergonomic Rust.

  As for smithay, runa's design philosophy is pretty different. First of all, smithay was created quite a few years back, and Rust has evolved a lot in that time. runa tries to take advantage of new Rust features, most notably `async`, as much as possible. (this does unfortunately mean a nightly Rust compiler is required for now, at least until TAIT is stabilized).

  Also, runa took a page from other amazing Rust crates like [smol](https://docs.rs/smol/latest/smol/) and [futures](https://docs.rs/futures/latest/futures), and has a very modular design, unlike smithay which is everything inside a single crate. This way 1) users can bring their own implementation if what we provide is not suitable; 2) some of our crates can be useful outside runa.
