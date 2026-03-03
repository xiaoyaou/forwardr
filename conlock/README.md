# 可用于同步或异步接收的一次性单值信道实现

> 本文中的bit组合位序使用计算机二进制反序表示：
> low 00000 high

### 原子状态位视图说明

| READY | CLOSE | WAITING | FIELDLESS | PENDING | Sender View | Receiver View |
|:-----:|:-----:|:-------:|:---------:|:-------:|-------------|---------------|
|   0   |   0   |    0    |     0     |    0    | 初始状态，可发送    | 初始状态，需等待      |
|   1   |   0   |    0    |     0     |    0    | 发送完毕可以关闭    | 数据就绪可以读取      |
|   0   |   0   |    1    |     0     |    0    | 可发送、同步唤醒    | 同步等待数据就绪      |
|   0   |   0   |    1    |     0     |    1    | 可发送、异步唤醒    | 异步等待数据就绪      |
|   0   |   0   |    1    |     1     |    1    | 正在设置：唤醒器    | 正在设置：唤醒器      |
|   0   |   0   |    0    |     1     |    0    | 已读取，还未关闭    | 已读取，不可再读      |
|   0   |   1   |    0    |     0     |    0    | 接收关闭、不可发    | 发送关闭、不可读      |
|   0   |   1   |    0    |     1     |    0    | 预备销毁、可回收    | 预备销毁、可回收      |
|   1   |   1   |    0    |     1     |    0    | 发送销毁、已结束    | 发送销毁、可读取      |

### 原子状态位转移表

|      bits      | ==> |      send      |      recv      |     await      |   pre del s    |    dropped     |   pre del r    |    dropped     |
|:--------------:|:---:|:--------------:|:--------------:|:--------------:|:--------------:|:--------------:|:--------------:|:--------------:|
| 00000<br/>初始状态 |  -  | 10000<br/>数据就绪 | 00100<br/>同步等待 | 001X1<br/>异步等待 |   -<br/>无需操作   | 01010<br/>发送销毁 |   -<br/>无需操作   | 01010<br/>接收销毁 |
| 10000<br/>数据就绪 |  -  |       X        | 00010<br/>数据已读 | 00010<br/>数据已读 |   -<br/>无需操作   | 11010<br/>发送销毁 | 01000<br/>清理关闭 | 01010<br/>接收销毁 |
| 00100<br/>同步等待 |  -  | 10000<br/>就绪唤醒 |   -<br/>重复等待   |       X        | 01000<br/>关闭唤醒 | 01010<br/>发送销毁 |       X        |       X        |
| 00111<br/>异步轮询 |  -  | 10000<br/>强制就绪 |       X        | 00101<br/>异步等待 |   -<br/>无需操作   | 01010<br/>强制销毁 |       X        |       X        |
| 00101<br/>异步等待 |  -  | 10000<br/>就绪唤醒 | 00100<br/>强制同步 | 00111<br/>重复轮询 | 01000<br/>关闭唤醒 | 01010<br/>发送销毁 | 01000<br/>关闭唤醒 | 01010<br/>接收销毁 |
| 01000<br/>通道关闭 |  -  |  Err<br/>禁止发送  | None<br/>无可读取  | None<br/>无可读取  |   -<br/>无需操作   | 01010<br/>发送销毁 |   -<br/>无需操作   | 01010<br/>接收销毁 |
| 01010<br/>预备销毁 |  -  |  Err<br/>禁止发送  | None<br/>无可读取  | None<br/>无可读取  |   -<br/>无需操作   |   -<br/>内存回收   |   -<br/>无需操作   |   -<br/>内存回收   |
| 00010<br/>数据已读 |  -  |       X        | None<br/>无可读取  | None<br/>无可读取  |   -<br/>无需操作   | 01010<br/>发送销毁 |   -<br/>无需操作   | 01010<br/>接收销毁 |
| 11010<br/>就绪销毁 |  -  |       X        | 01010<br/>已读关闭 | 01010<br/>已读关闭 |       X        |       X        | 01010<br/>清理销毁 |   -<br/>内存回收   |




### 原子状态位转换流程图

```mermaid
---
title: 全局信道状态图
---
stateDiagram-v2
    new: <center>00000<br/>(new)</center>
    ready: <center>10000<br/>(ready)</center>
    waiting: <center>00100<br/>(waiting)</center>
    pending: <center>00101<br/>(pending)</center>
    polling: <center>00111<br/>(polling)</center>
    dataless: <center>00010<br/>(dataless)</center>
    closed: <center>01000<br/>(closed)</center>
    droppable: <center>11010<br/>(droppable)</center>
    dropped: <center>01010<br/>(dropped)</center>

    [*] --> new: alloc
    new --> dropped: del s
    new --> dropped: del r
    new --> ready: send
    new --> waiting: recv
    new --> async: await

    ready --> dataless: recv
    ready --> dataless: await
    ready --> droppable: del s
    ready --> closed: pre del r

    waiting --> ready: send
    waiting --> waiting: recv
    waiting --> closed: pre del s

    pending --> waiting: recv
    async --> ready: send
    pending --> closed: pre del s
    pending --> closed: pre del r

    dataless --> dropped: del s
    dataless --> dropped: del r

    droppable --> dropped: recv
    droppable --> dropped: await
    droppable --> dropped: pre del r

    closed --> dropped: del s
    closed --> dropped: del r

    polling --> dropped: del s

    dropped --> [*]: dealloc

    state async {
        [*] --> polling: poll
        polling --> pending: polled
        pending --> polling: poll
    }
```
