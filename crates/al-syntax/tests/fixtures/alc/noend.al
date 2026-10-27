codeunit 50141 "AZJ Journey Mgt"
{
    procedure AddItem(ItemCode: Code[20]; NewDescription: Text[100]; NewQuantity: Decimal; CustomerNo: Code[20])
    var
        JourneyItem: Record "AZJ Journey Item";
    begin
        JourneyItem.Init();
        JourneyItem.Code := ItemCode;
        JourneyItem.Description := NewDescription;
        JourneyItem.Quantity := NewQuantity;
        JourneyItem."Customer No." := CustomerNo;
        JourneyItem.Insert(true);

    procedure CountForCustomer(CustomerNo: Code[20]): Integer
    var
        JourneyItem: Record "AZJ Journey Item";
    begin
        JourneyItem.SetRange("Customer No.", CustomerNo);
        exit(JourneyItem.Count());
    end;

    procedure Double(Value: Integer): Integer
    begin
        exit(Value * 2);
    end;
}
