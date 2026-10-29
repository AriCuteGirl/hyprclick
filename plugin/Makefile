CXXFLAGS ?= -O2 -g
PKGS = pixman-1 libdrm hyprland pangocairo libinput libudev wayland-server xkbcommon

hyprclick.so: hyprclick.cpp
	$(CXX) -shared -fPIC -std=c++2b $(CXXFLAGS) $< -o $@ $(shell pkg-config --cflags $(PKGS))

clean:
	rm -f hyprclick.so

.PHONY: clean
