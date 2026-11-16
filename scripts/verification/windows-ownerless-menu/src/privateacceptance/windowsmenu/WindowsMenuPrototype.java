package privateacceptance.windowsmenu;

import com.intellij.ide.IdeEventQueue;
import com.intellij.openapi.Disposable;
import com.intellij.openapi.components.Service;
import com.intellij.openapi.project.Project;
import com.intellij.openapi.util.Disposer;
import com.intellij.openapi.wm.*;
import java.awt.*;
import java.awt.event.*;
import java.awt.image.BufferedImage;
import java.nio.charset.StandardCharsets;
import java.nio.file.*;
import java.time.Instant;
import java.util.*;
import java.util.List;
import javax.swing.*;
import javax.swing.border.EmptyBorder;

/**
 * Windows-only diagnostic prototype, NOT a product tray surface.
 * The only custom top-levels are the actual menu and its actual submenu.
 * Public JDK widgets/APIs and the pre-existing own-TrayIcon SDK bridge only.
 * No IDE window is used as an owner or activated/hidden/disposed by this class.
 */
@Service(Service.Level.PROJECT)
public final class WindowsMenuPrototype implements Disposable {
    private static final String TOOL = "Windows独自menu診断";
    private final Project project;
    private final Path trace = Path.of(System.getProperty("prototype.trace"));
    private final Path state = Path.of(System.getProperty("prototype.state"));
    private final JLabel status = new JLabel();
    private final boolean[] checked = {true, false};
    private Disposable life;
    private TrayIcon tray;
    private MenuWindow root;
    private boolean started, stopped;
    private int callbacks, shows, closes;
    private String last = "none";

    public WindowsMenuPrototype(Project project) { this.project = project; }

    public void start() {
        if (started) return;
        if (!System.getProperty("os.name").startsWith("Windows"))
            throw new IllegalStateException("Windows専用prototypeです");
        if (!SystemTray.isSupported()) throw new IllegalStateException("SystemTray非対応");
        started = true;
        life = Disposer.newDisposable(this, "Windows ownerless menu prototype lifetime");
        JPanel panel = new JPanel(new GridLayout(0, 1, 0, 6));
        panel.add(new JLabel("製品A/Bなし・dummyのみ。実trayのWをダブルクリックして開始"));
        panel.add(status);
        JButton stop = new JButton("試作終了：自menu／tray／listenerだけを除去");
        stop.addActionListener(e -> stopSession());
        panel.add(stop);
        ToolWindowManager.getInstance(project).registerToolWindow(TOOL, panel, ToolWindowAnchor.BOTTOM).activate(null);
        BufferedImage image = new BufferedImage(32, 32, BufferedImage.TYPE_INT_ARGB);
        Graphics2D g = image.createGraphics();
        g.setColor(new Color(0x005f73)); g.fillRect(0, 0, 32, 32);
        g.setColor(Color.WHITE); g.setFont(new Font("Dialog", Font.BOLD, 25)); g.drawString("W", 2, 25); g.dispose();
        tray = new TrayIcon(image, "Windows独自menu試作 / double-click W");
        tray.setImageAutoSize(true);
        tray.addActionListener(e -> {
            if (!stopped) SwingUtilities.invokeLater(this::openFromTray);
        });
        // Exact existing bridge policy: one own current TrayIcon, same event, no re-post.
        IdeEventQueue.getInstance().addPostEventListener(event -> {
            if (!stopped && event instanceof InvocationEvent && event.getSource() == tray)
                event.setSource(Toolkit.getDefaultToolkit());
            return false;
        }, life);
        try { SystemTray.getSystemTray().add(tray); }
        catch (AWTException e) { throw new IllegalStateException(e); }
        record("START java=" + System.getProperty("java.runtime.version") + " locale=" + Locale.getDefault().toLanguageTag());
        publish();
    }

    private void openFromTray() {
        if (stopped) return;
        if (root != null) { record("ALREADY_OPEN ignored"); return; }
        Point p = MouseInfo.getPointerInfo().getLocation();
        root = new MenuWindow(null, -1, rootRows());
        root.showAt(p.x - root.getWidth() + 20, p.y - root.getHeight() - 18);
        shows++;
        record("OPEN realTray=true rootOwnerNull=" + (root.getOwner() == null)
            + " size=" + root.getWidth() + "x" + root.getHeight()
            + " opacity=" + root.getOpacity() + " toolkitDpi=" + Toolkit.getDefaultToolkit().getScreenResolution()
            + " transform=" + root.getGraphicsConfiguration().getDefaultTransform());
        publish();
    }

    List<Row> rootRows() {
        return List.of(
            Row.info("Android Replay Buffer — 独立診断 / dummy"),
            Row.separator(),
            Row.submenu("日本語 Project " + captureStatus(0), 0),
            Row.submenu("Project ASCII " + captureStatus(1), 1)
        );
    }

    private String captureStatus(int index) {
        return checked[index] ? "● 取得中" : "○ 取得OFF";
    }

    List<Row> projectRows(int index) {
        return List.of(
            Row.info(captureStatus(index) + (checked[index] ? "（合成状態・dummy）" : "（dummy）")),
            Row.separator(),
            Row.command("直前180秒を保存", "SAVE", true, index),
            Row.command("スクリーンショット — 診断でdisabled", "SCREENSHOT", false, index),
            Row.separator(),
            Row.command("保存しました 12:34 — フォルダを開く", "OPEN_DIRECTORY", true, index),
            Row.separator(),
            Row.toggle("取得する（OFFで未保存の記録を破棄）", checked[index], index),
            Row.separator(),
            Row.command("Android Studioで表示", "SHOW_IDE", true, index),
            Row.command("設定…", "OPEN_SETTINGS", true, index)
        );
    }

    private boolean ownMenu(Window w) {
        for (Window p = w; p != null; p = p.getOwner()) if (p == root) return true;
        return false;
    }

    void command(Row row) {
        if (!row.enabled || row.command == null) return;
        if (row.command.equals("TOGGLE_CAPTURE")) checked[row.project] = !checked[row.project];
        callbacks++;
        last = row.command + " / project=" + row.project;
        record("CALLBACK count=" + callbacks + " command=" + row.command
            + " project=" + row.project + " checked=" + checked[row.project]);
        // ALL commands are dummy, including SHOW_IDE/OPEN_SETTINGS. Never route into the IDE.
        closeAll("command");
    }

    private void closeAll(String reason) {
        if (root == null) return;
        MenuWindow old = root;
        root = null;
        old.disposeTree();
        closes++;
        record("CLOSE reason=" + reason + " showingMenus=" + showingMenus());
        publish();
    }

    private int showingMenus() {
        int n = 0;
        for (Window w : Window.getWindows()) if (w instanceof MenuWindow && w.isDisplayable()) n++;
        return n;
    }

    private void record(String line) {
        try {
            Files.writeString(trace, Instant.now() + " " + line + "\n", StandardCharsets.UTF_8,
                StandardOpenOption.CREATE, StandardOpenOption.APPEND);
        } catch (Exception e) { throw new IllegalStateException(e); }
    }

    private void publish() {
        status.setText("callback=" + callbacks + " / last=" + last + " / opened=" + shows + " closed=" + closes);
        String json = "{\"callbacks\":" + callbacks + ",\"last\":\"" + last + "\",\"shows\":" + shows
            + ",\"closes\":" + closes + ",\"checked\":[" + checked[0] + "," + checked[1]
            + "],\"menuWindows\":" + showingMenus() + ",\"stopped\":" + stopped + "}";
        try {
            Files.writeString(state, json, StandardCharsets.UTF_8, StandardOpenOption.CREATE, StandardOpenOption.TRUNCATE_EXISTING);
        } catch (Exception e) { throw new IllegalStateException(e); }
    }

    public void stopSession() {
        if (!started || stopped) return;
        stopped = true;
        closeAll("cleanup");
        SystemTray.getSystemTray().remove(tray);
        Disposer.dispose(life);
        ToolWindowManager.getInstance(project).unregisterToolWindow(TOOL);
        record("CLEANUP menuWindows=" + showingMenus() + " trayAbsent=" + Arrays.stream(SystemTray.getSystemTray().getTrayIcons()).noneMatch(i -> i == tray)
            + " listenerLifetimeDisposed=" + Disposer.isDisposed(life));
        publish();
    }

    @Override public void dispose() { stopSession(); }

    static final class Row {
        final String label, command;
        final boolean enabled, separator, toggle, checked;
        final int project;
        private Row(String label, String command, boolean enabled, boolean separator, boolean toggle, int project) {
            this(label, command, enabled, separator, toggle, project, false);
        }
        private Row(String label, String command, boolean enabled, boolean separator, boolean toggle, int project, boolean checked) {
            this.label=label; this.command=command; this.enabled=enabled; this.separator=separator; this.toggle=toggle; this.project=project;
            this.checked=checked;
        }
        static Row info(String s) { return new Row(s,null,false,false,false,-1); }
        static Row separator() { return new Row("",null,false,true,false,-1); }
        static Row submenu(String s,int p) { return new Row(s,null,true,false,false,p); }
        static Row command(String s,String c,boolean e,int p) { return new Row(s,c,e,false,false,p); }
        static Row toggle(String s,boolean checked,int p) { return new Row(s,"TOGGLE_CAPTURE",true,false,true,p,checked); }
        boolean submenu() { return enabled && command == null; }
        @Override public String toString() { return label; }
    }

    private final class MenuWindow extends JDialog {
        final MenuWindow parentMenu;
        final JList<Row> list;
        MenuWindow child;
        boolean disposing;
        MenuWindow(MenuWindow parent, int projectIndex, List<Row> rows) {
            // Window-null overload: true ownerless root; NO shared Frame or anchor.
            super((Window)parent, Dialog.ModalityType.MODELESS);
            parentMenu = parent;
            setTitle(parent == null ? "Windows独自menu試作" : "Windows独自submenu試作");
            setUndecorated(true);
            setType(Window.Type.POPUP);
            setDefaultCloseOperation(WindowConstants.DO_NOTHING_ON_CLOSE);
            setFocusableWindowState(true);
            setAutoRequestFocus(true);
            list = new JList<>(rows.toArray(Row[]::new));
            list.setSelectionMode(ListSelectionModel.SINGLE_SELECTION);
            list.setBackground(new Color(0x252932));
            list.setForeground(new Color(0xf0f0f0));
            list.setFont(new Font("Dialog", Font.PLAIN, 15));
            list.setCellRenderer(new MenuRenderer());
            JPanel body = new JPanel(new BorderLayout());
            body.setBorder(BorderFactory.createCompoundBorder(BorderFactory.createLineBorder(new Color(0x727984)),
                new EmptyBorder(6,6,6,6)));
            body.setBackground(list.getBackground());
            body.add(list, BorderLayout.CENTER);
            JLabel footer = new JLabel("dummy callback=" + callbacks + " / last=" + last + "  |  Escで閉じる");
            footer.setForeground(new Color(0xcbd5e1)); footer.setFont(list.getFont().deriveFont(12f));
            footer.setBorder(new EmptyBorder(7,6,3,6)); body.add(footer,BorderLayout.SOUTH);
            setContentPane(body);
            bind("UP", () -> move(-1));
            bind("DOWN", () -> move(1));
            bind("ENTER", this::activateSelected);
            bind("SPACE", this::activateSelected);
            bind("RIGHT", this::activateSubmenu);
            bind("LEFT", () -> { if (parentMenu != null) { parentMenu.closeChild(); parentMenu.list.requestFocusInWindow(); } });
            bind("ESCAPE", () -> closeAll("escape"));
            list.addMouseMotionListener(new MouseMotionAdapter() {
                @Override public void mouseMoved(MouseEvent e) {
                    int i = rowAt(e.getPoint());
                    if (i >= 0 && list.getModel().getElementAt(i).enabled) list.setSelectedIndex(i);
                }
            });
            list.addMouseListener(new MouseAdapter() {
                @Override public void mouseReleased(MouseEvent e) {
                    if (!SwingUtilities.isLeftMouseButton(e)) return;
                    int i = rowAt(e.getPoint());
                    if (i >= 0 && list.getModel().getElementAt(i).enabled) {
                        list.setSelectedIndex(i); activateSelected();
                    } else {
                        record("DISABLED_CLICK noCallback=true");
                    }
                }
            });
            addWindowFocusListener(new WindowAdapter() {
                @Override public void windowGainedFocus(WindowEvent e) { list.requestFocusInWindow(); }
                @Override public void windowLostFocus(WindowEvent e) {
                    if (!disposing && root != null && !ownMenu(e.getOppositeWindow()))
                        closeAll("focus_lost");
                }
            });
            pack();
            move(1);
        }
        void bind(String key, Runnable action) {
            MenuKeyBindings.bind(list,key,new AbstractAction() {
                @Override public void actionPerformed(ActionEvent e) { record("KEY " + key); action.run(); }
            });
        }
        int rowAt(Point p) {
            int i=list.locationToIndex(p);
            return i>=0 && list.getCellBounds(i,i).contains(p) ? i : -1;
        }
        void move(int d) {
            int n=list.getModel().getSize(), start=list.getSelectedIndex();
            for (int j=1;j<=n;j++) {
                int i=Math.floorMod(start+d*j,n);
                if (list.getModel().getElementAt(i).enabled) { list.setSelectedIndex(i); return; }
            }
        }
        void activateSelected() {
            Row row=list.getSelectedValue();
            if(row==null || !row.enabled)return;
            if(row.submenu()) activateSubmenu(); else command(row);
        }
        void activateSubmenu() {
            Row row=list.getSelectedValue();
            if(row==null || !row.submenu())return;
            closeChild();
            child=new MenuWindow(this,row.project,projectRows(row.project));
            Rectangle cell=list.getCellBounds(list.getSelectedIndex(),list.getSelectedIndex());
            Point p=list.getLocationOnScreen();
            Rectangle screen=usable(getGraphicsConfiguration());
            int x=getX()+getWidth()-1;
            if (x+child.getWidth()>screen.x+screen.width) x=getX()-child.getWidth()+1;
            child.showAt(x,p.y+cell.y-6);
            record("SUBMENU ownerIsActualMenu=" + (child.getOwner()==this));
            publish();
        }
        void closeChild() {
            if(child!=null) { MenuWindow old=child; child=null; old.disposeTree(); publish(); }
        }
        void disposeTree() { disposing=true; closeChild(); dispose(); }
        void showAt(int x,int y) {
            Rectangle usable=usable(getGraphicsConfiguration());
            x=Math.max(usable.x,Math.min(x,usable.x+usable.width-getWidth()));
            y=Math.max(usable.y,Math.min(y,usable.y+usable.height-getHeight()));
            setLocation(x,y);
            setVisible(true);
            list.requestFocusInWindow(); // Normal focus of actual menu only. No owner/IDE activation.
        }
    }

    private static Rectangle usable(GraphicsConfiguration gc) {
        Rectangle r=new Rectangle(gc.getBounds());
        Insets i=Toolkit.getDefaultToolkit().getScreenInsets(gc);
        r.x+=i.left; r.y+=i.top; r.width-=i.left+i.right; r.height-=i.top+i.bottom;
        return r;
    }

    /** Vector check/arrow/separator; normal composite-font labels support Japanese/ASCII. */
    private final class MenuRenderer extends JPanel implements ListCellRenderer<Row> {
        final JLabel label=new JLabel();
        Row row;
        MenuRenderer() { super(new BorderLayout()); setOpaque(true); add(label); }
        @Override public Component getListCellRendererComponent(JList<? extends Row> l,Row r,int index,boolean selected,boolean focused) {
            row=r;
            label.setText(r.label);
            label.setFont(l.getFont());
            label.setForeground(r.enabled ? new Color(0xf1f5f9) : new Color(0x9ca3af));
            setBackground(selected && r.enabled ? new Color(0x305d85) : l.getBackground());
            setBorder(new EmptyBorder(r.separator ? 4 : 7,30,r.separator ? 4 : 7,25));
            setPreferredSize(r.separator ? new Dimension(20,9) : new Dimension(label.getPreferredSize().width+75,34));
            return this;
        }
        @Override protected void paintComponent(Graphics graphics) {
            super.paintComponent(graphics);
            Graphics2D g=(Graphics2D)graphics.create();
            g.setRenderingHint(RenderingHints.KEY_ANTIALIASING,RenderingHints.VALUE_ANTIALIAS_ON);
            g.setColor(new Color(0xcbd5e1));
            int y=getHeight()/2;
            if(row.separator) { g.setColor(new Color(0x505761)); g.drawLine(6,y,getWidth()-6,y); }
            if(row.toggle) {
                g.drawRect(8,y-6,12,12);
                if(row.checked) { g.setStroke(new BasicStroke(2)); g.drawLine(10,y,14,y+4); g.drawLine(14,y+4,19,y-4); }
            }
            if(row.submenu()) { int x=getWidth()-12; g.fillPolygon(new int[]{x-4,x+1,x-4},new int[]{y-5,y,y+5},3); }
            g.dispose();
        }
    }
}
