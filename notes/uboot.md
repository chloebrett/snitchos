U-Boot, from fresh:
dhcp
setenv serverip 192.168.0.7
setenv fdt_high 0x48000000
tftpboot 0x40200000 snitchos.img
setenv bootargs 'workload=stitch-drivel'
booti 0x40200000 - ${fdtcontroladdr}
