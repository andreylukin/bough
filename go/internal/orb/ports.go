package orb

import (
	"context"
	"fmt"
	"net"
	"strconv"
	"strings"
	"time"

	"github.com/andreylukin/bough/internal/container"
)

// planPortsLocked decides a new container's forwards from project.yml: a
// host port already in use (another orb, a dev server) is left out with a
// reason, so parallel orbs of one project still start.
func (o *Orb) planPortsLocked() {
	o.state.Ports, o.spec.Ports = nil, nil
	for _, p := range o.project.Def.Ports {
		ps := PortState{Host: p.Host, Guest: p.Guest}
		if hostPortBusy(p.Host) {
			ps.Error = fmt.Sprintf("127.0.0.1:%d is in use on the host", p.Host)
		} else {
			o.spec.Ports = append(o.spec.Ports, container.PortMap{Host: p.Host, Guest: p.Guest})
		}
		o.state.Ports = append(o.state.Ports, ps)
	}
}

// keepPortsLocked is a reused container's forwards: the ones it was
// created with, as the last state recorded them.
func (o *Orb) keepPortsLocked(prev []PortState) {
	o.state.Ports, o.spec.Ports = prev, nil
	for _, p := range prev {
		if p.Error == "" {
			o.spec.Ports = append(o.spec.Ports, container.PortMap{Host: p.Host, Guest: p.Guest})
		}
	}
}

// hostPortBusy: something accepts on the port, or it cannot be bound.
// Both checks, because a wildcard listener does not stop a loopback bind.
func hostPortBusy(port int) bool {
	addr := net.JoinHostPort("127.0.0.1", strconv.Itoa(port))
	if c, err := net.DialTimeout("tcp", addr, 200*time.Millisecond); err == nil {
		c.Close()
		return true
	}
	l, err := net.Listen("tcp", addr)
	if err != nil {
		return true
	}
	l.Close()
	return false
}

// startErr names the forwarded ports when the runtime could not bind one.
func startErr(err error, ports []container.PortMap) error {
	if err == nil || len(ports) == 0 || !strings.Contains(strings.ToLower(err.Error()), "address already in use") {
		return err
	}
	names := make([]string, len(ports))
	for i, p := range ports {
		names[i] = fmt.Sprintf("127.0.0.1:%d", p.Host)
	}
	return fmt.Errorf("a forwarded host port (%s) is in use: stop what holds it, or change ports: in project.yml and remove this orb to recreate it: %w", strings.Join(names, ", "), err)
}

// addressLocked records the running container's IP; "" when the runtime
// cannot say.
func (o *Orb) addressLocked(ctx context.Context) {
	o.state.IP = ""
	if a, ok := o.rt.(container.Addresser); ok {
		if ip, err := a.Address(ctx, o.spec.Name); err == nil {
			o.state.IP = ip
		}
	}
}
